/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*-
 * vim:cindent:ts=2:et:sw=2:
 *
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 *
 * This Original Code has been modified by IBM Corporation. Modifications made by IBM 
 * described herein are Copyright (c) International Business Machines Corporation, 2000.
 * Modifications to Mozilla code or documentation identified per MPL Section 3.3
 *
 * Date             Modified by     Description of modification
 * 04/20/2000       IBM Corp.      OS/2 VisualAge build.
 */

/* storage of the frame tree and information about it */

#include "mozilla/DebugOnly.h"

#include "nscore.h"
#include "nsPresContext.h"
#include "nsIPresShell.h"
#include "nsStyleSet.h"
#include "nsCSSFrameConstructor.h"
#include "nsStyleContext.h"
#include "nsStyleChangeList.h"
#include "nsIServiceManager.h"
#include "nsCOMPtr.h"
#include "prthread.h"
#include "plhash.h"
#include "nsPlaceholderFrame.h"
#include "nsGkAtoms.h"
#include "nsCSSPseudoElements.h"
#ifdef DEBUG
#include "nsIStyleRule.h"
#endif
#include "nsILayoutHistoryState.h"
#include "nsPresState.h"
#include "nsIContent.h"
#include "nsINameSpaceManager.h"
#include "nsIDocument.h"
#include "nsIScrollableFrame.h"

#include "nsIDOMNodeList.h"
#include "nsIDOMHTMLCollection.h"
#include "nsIFormControl.h"
#include "nsIDOMElement.h"
#include "nsIDOMHTMLFormElement.h"
#include "nsIForm.h"
#include "nsContentUtils.h"
#include "nsReadableUtils.h"
#include "nsUnicharUtils.h"
#include "nsError.h"
#include "nsLayoutUtils.h"
#include "nsAutoPtr.h"
#include "imgIRequest.h"
#include "nsAbsoluteContainingBlock.h"
#include "ChildIterator.h"

#include "nsFrameManager.h"
#include "GeckoProfiler.h"

  #ifdef DEBUG
    //#define DEBUG_UNDISPLAYED_MAP
  #else
    #undef DEBUG_UNDISPLAYED_MAP
  #endif

using namespace mozilla;
using namespace mozilla::dom;

//----------------------------------------------------------------------

struct PlaceholderMapEntry : public PLDHashEntryHdr {
  // key (the out of flow frame) can be obtained through placeholder frame
  nsPlaceholderFrame *placeholderFrame;
};

static bool
PlaceholderMapMatchEntry(PLDHashTable *table, const PLDHashEntryHdr *hdr,
                         const void *key)
{
  const PlaceholderMapEntry *entry =
    static_cast<const PlaceholderMapEntry*>(hdr);
  NS_ASSERTION(entry->placeholderFrame->GetOutOfFlowFrame() !=
               (void*)0xdddddddd,
               "Dead placeholder in placeholder map");
  return entry->placeholderFrame->GetOutOfFlowFrame() == key;
}

static PLDHashTableOps PlaceholderMapOps = {
  PL_DHashAllocTable,
  PL_DHashFreeTable,
  PL_DHashVoidPtrKeyStub,
  PlaceholderMapMatchEntry,
  PL_DHashMoveEntryStub,
  PL_DHashClearEntryStub,
  PL_DHashFinalizeStub,
  NULL
};

//----------------------------------------------------------------------

// XXXldb This seems too complicated for what I think it's doing, and it
// should also be using pldhash rather than plhash to use less memory.

class nsFrameManagerBase::UndisplayedMap {
public:
  UndisplayedMap(uint32_t aNumBuckets = 16) NS_HIDDEN;
  ~UndisplayedMap(void) NS_HIDDEN;

  NS_HIDDEN_(UndisplayedNode*) GetFirstNode(nsIContent* aParentContent);

  NS_HIDDEN_(nsresult) AddNodeFor(nsIContent* aParentContent,
                                  nsIContent* aChild, nsStyleContext* aStyle);

  NS_HIDDEN_(void) RemoveNodeFor(nsIContent* aParentContent,
                                 UndisplayedNode* aNode);

  NS_HIDDEN_(void) RemoveNodesFor(nsIContent* aParentContent);

  // Removes all entries from the hash table
  NS_HIDDEN_(void)  Clear(void);

protected:
  /**
   * Gets the entry for the provided parent content. If the content
   * is a <xbl:children> element, |**aParentContent| is set to
   * the parent of the children element.
   */
  NS_HIDDEN_(PLHashEntry**) GetEntryFor(nsIContent** aParentContent);
  NS_HIDDEN_(void)          AppendNodeFor(UndisplayedNode* aNode,
                                          nsIContent* aParentContent);

  PLHashTable*  mTable;
  PLHashEntry** mLastLookup;
};

//----------------------------------------------------------------------

nsFrameManager::~nsFrameManager()
{
  NS_ASSERTION(!mPresShell, "nsFrameManager::Destroy never called");
}

void
nsFrameManager::Destroy()
{
  NS_ASSERTION(mPresShell, "Frame manager already shut down.");

  // Destroy the frame hierarchy.
  mPresShell->SetIgnoreFrameDestruction(true);

  // Unregister all placeholders before tearing down the frame tree
  nsFrameManager::ClearPlaceholderFrameMap();

  if (mRootFrame) {
    mRootFrame->Destroy();
    mRootFrame = nullptr;
  }
  
  delete mUndisplayedMap;
  mUndisplayedMap = nullptr;

  mPresShell = nullptr;
}

//----------------------------------------------------------------------

// Placeholder frame functions
nsPlaceholderFrame*
nsFrameManager::GetPlaceholderFrameFor(const nsIFrame* aFrame)
{
  NS_PRECONDITION(aFrame, "null param unexpected");

  if (mPlaceholderMap.ops) {
    PlaceholderMapEntry *entry = static_cast<PlaceholderMapEntry*>
                                            (PL_DHashTableOperate(const_cast<PLDHashTable*>(&mPlaceholderMap),
                                aFrame, PL_DHASH_LOOKUP));
    if (PL_DHASH_ENTRY_IS_BUSY(entry)) {
      return entry->placeholderFrame;
    }
  }

  return nullptr;
}

nsresult
nsFrameManager::RegisterPlaceholderFrame(nsPlaceholderFrame* aPlaceholderFrame)
{
  NS_PRECONDITION(aPlaceholderFrame, "null param unexpected");
  NS_PRECONDITION(nsGkAtoms::placeholderFrame == aPlaceholderFrame->GetType(),
                  "unexpected frame type");
  if (!mPlaceholderMap.ops) {
    if (!PL_DHashTableInit(&mPlaceholderMap, &PlaceholderMapOps, nullptr,
                           sizeof(PlaceholderMapEntry), 16)) {
      mPlaceholderMap.ops = nullptr;
      return NS_ERROR_OUT_OF_MEMORY;
    }
  }
  PlaceholderMapEntry *entry = static_cast<PlaceholderMapEntry*>(PL_DHashTableOperate(&mPlaceholderMap,
                              aPlaceholderFrame->GetOutOfFlowFrame(),
                              PL_DHASH_ADD));
  if (!entry)
    return NS_ERROR_OUT_OF_MEMORY;

  NS_ASSERTION(!entry->placeholderFrame, "Registering a placeholder for a frame that already has a placeholder!");
  entry->placeholderFrame = aPlaceholderFrame;

  return NS_OK;
}

void
nsFrameManager::UnregisterPlaceholderFrame(nsPlaceholderFrame* aPlaceholderFrame)
{
  NS_PRECONDITION(aPlaceholderFrame, "null param unexpected");
  NS_PRECONDITION(nsGkAtoms::placeholderFrame == aPlaceholderFrame->GetType(),
                  "unexpected frame type");

  if (mPlaceholderMap.ops) {
    PL_DHashTableOperate(&mPlaceholderMap,
                         aPlaceholderFrame->GetOutOfFlowFrame(),
                         PL_DHASH_REMOVE);
  }
}

static PLDHashOperator
UnregisterPlaceholders(PLDHashTable* table, PLDHashEntryHdr* hdr,
                       uint32_t number, void* arg)
{
  PlaceholderMapEntry* entry = static_cast<PlaceholderMapEntry*>(hdr);
  entry->placeholderFrame->SetOutOfFlowFrame(nullptr);
  return PL_DHASH_NEXT;
}

void
nsFrameManager::ClearPlaceholderFrameMap()
{
  if (mPlaceholderMap.ops) {
    PL_DHashTableEnumerate(&mPlaceholderMap, UnregisterPlaceholders, nullptr);
    PL_DHashTableFinish(&mPlaceholderMap);
    mPlaceholderMap.ops = nullptr;
  }
}

//----------------------------------------------------------------------

nsStyleContext*
nsFrameManager::GetUndisplayedContent(nsIContent* aContent)
{
  if (!aContent || !mUndisplayedMap)
    return nullptr;

  nsIContent* parent = aContent->GetParent();
  for (UndisplayedNode* node = mUndisplayedMap->GetFirstNode(parent);
         node; node = node->mNext) {
    if (node->mContent == aContent)
      return node->mStyle;
  }

  return nullptr;
}

UndisplayedNode*
nsFrameManager::GetAllUndisplayedContentIn(nsIContent* aParentContent)
{
  if (!mUndisplayedMap)
    return nullptr;

  return mUndisplayedMap->GetFirstNode(aParentContent);
}

void
nsFrameManager::SetUndisplayedContent(nsIContent* aContent, 
                                      nsStyleContext* aStyleContext)
{
  NS_PRECONDITION(!aStyleContext->GetPseudo(),
                  "Should only have actual elements here");

#ifdef DEBUG_UNDISPLAYED_MAP
  static int i = 0;
  printf("SetUndisplayedContent(%d): p=%p \n", i++, (void *)aContent);
#endif

  NS_ASSERTION(!GetUndisplayedContent(aContent),
               "Already have an undisplayed context entry for aContent");

  if (! mUndisplayedMap) {
    mUndisplayedMap = new UndisplayedMap;
  }
  nsIContent* parent = aContent->GetParent();
  NS_ASSERTION(parent || (mPresShell && mPresShell->GetDocument() &&
               mPresShell->GetDocument()->GetRootElement() == aContent),
               "undisplayed content must have a parent, unless it's the root "
               "element");
  mUndisplayedMap->AddNodeFor(parent, aContent, aStyleContext);
}

void
nsFrameManager::ChangeUndisplayedContent(nsIContent* aContent, 
                                         nsStyleContext* aStyleContext)
{
  NS_ASSERTION(mUndisplayedMap, "no existing undisplayed content");
  
#ifdef DEBUG_UNDISPLAYED_MAP
   static int i = 0;
   printf("ChangeUndisplayedContent(%d): p=%p \n", i++, (void *)aContent);
#endif

  for (UndisplayedNode* node = mUndisplayedMap->GetFirstNode(aContent->GetParent());
         node; node = node->mNext) {
    if (node->mContent == aContent) {
      node->mStyle = aStyleContext;
      return;
    }
  }

  NS_NOTREACHED("no existing undisplayed content");
}

void
nsFrameManager::ClearUndisplayedContentIn(nsIContent* aContent,
                                          nsIContent* aParentContent)
{
#ifdef DEBUG_UNDISPLAYED_MAP
  static int i = 0;
  printf("ClearUndisplayedContent(%d): content=%p parent=%p --> ", i++, (void *)aContent, (void*)aParentContent);
#endif
  
  if (mUndisplayedMap) {
    UndisplayedNode* node = mUndisplayedMap->GetFirstNode(aParentContent);
    while (node) {
      if (node->mContent == aContent) {
        mUndisplayedMap->RemoveNodeFor(aParentContent, node);

#ifdef DEBUG_UNDISPLAYED_MAP
        printf( "REMOVED!\n");
#endif
#ifdef DEBUG
        // make sure that there are no more entries for the same content
        nsStyleContext *context = GetUndisplayedContent(aContent);
        NS_ASSERTION(context == nullptr, "Found more undisplayed content data after removal");
#endif
        return;
      }
      node = node->mNext;
    }
  }
}

void
nsFrameManager::ClearAllUndisplayedContentIn(nsIContent* aParentContent)
{
#ifdef DEBUG_UNDISPLAYED_MAP
  static int i = 0;
  printf("ClearAllUndisplayedContentIn(%d): parent=%p \n", i++, (void*)aParentContent);
#endif

  if (mUndisplayedMap) {
    mUndisplayedMap->RemoveNodesFor(aParentContent);
  }

  // Need to look at aParentContent's content list due to XBL insertions.
  // Nodes in aParentContent's content list do not have aParentContent as a
  // parent, but are treated as children of aParentContent. We iterate over
  // the flattened content list and just ignore any nodes we don't care about.
  FlattenedChildIterator iter(aParentContent);
  for (nsIContent* child = iter.GetNextChild(); child; child = iter.GetNextChild()) {
    if (child->GetParent() != aParentContent) {
      ClearUndisplayedContentIn(child, child->GetParent());
    }
  }
}

//----------------------------------------------------------------------
nsresult
nsFrameManager::AppendFrames(nsIFrame*       aParentFrame,
                             ChildListID     aListID,
                             nsFrameList&    aFrameList)
{
  if (aParentFrame->IsAbsoluteContainer() &&
      aListID == aParentFrame->GetAbsoluteListID()) {
    return aParentFrame->GetAbsoluteContainingBlock()->
           AppendFrames(aParentFrame, aListID, aFrameList);
  } else {
    return aParentFrame->AppendFrames(aListID, aFrameList);
  }
}

nsresult
nsFrameManager::InsertFrames(nsIFrame*       aParentFrame,
                             ChildListID     aListID,
                             nsIFrame*       aPrevFrame,
                             nsFrameList&    aFrameList)
{
  NS_PRECONDITION(!aPrevFrame || (!aPrevFrame->GetNextContinuation()
                  || (((aPrevFrame->GetNextContinuation()->GetStateBits() & NS_FRAME_IS_OVERFLOW_CONTAINER))
                  && !(aPrevFrame->GetStateBits() & NS_FRAME_IS_OVERFLOW_CONTAINER))),
                  "aPrevFrame must be the last continuation in its chain!");

  if (aParentFrame->IsAbsoluteContainer() &&
      aListID == aParentFrame->GetAbsoluteListID()) {
    return aParentFrame->GetAbsoluteContainingBlock()->
           InsertFrames(aParentFrame, aListID, aPrevFrame, aFrameList);
  } else {
    return aParentFrame->InsertFrames(aListID, aPrevFrame, aFrameList);
  }
}

nsresult
nsFrameManager::RemoveFrame(ChildListID     aListID,
                            nsIFrame*       aOldFrame)
{
  bool wasDestroyingFrames = mIsDestroyingFrames;
  mIsDestroyingFrames = true;

  // In case the reflow doesn't invalidate anything since it just leaves
  // a gap where the old frame was, we invalidate it here.  (This is
  // reasonably likely to happen when removing a last child in a way
  // that doesn't change the size of the parent.)
  // This has to sure to invalidate the entire overflow rect; this
  // is important in the presence of absolute positioning
  aOldFrame->InvalidateFrameForRemoval();

  NS_ASSERTION(!aOldFrame->GetPrevContinuation() ||
               // exception for nsCSSFrameConstructor::RemoveFloatingFirstLetterFrames
               aOldFrame->GetType() == nsGkAtoms::textFrame,
               "Must remove first continuation.");
  NS_ASSERTION(!(aOldFrame->GetStateBits() & NS_FRAME_OUT_OF_FLOW &&
                 GetPlaceholderFrameFor(aOldFrame)),
               "Must call RemoveFrame on placeholder for out-of-flows.");
  nsresult rv = NS_OK;
  nsIFrame* parentFrame = aOldFrame->GetParent();
  if (parentFrame->IsAbsoluteContainer() &&
      aListID == parentFrame->GetAbsoluteListID()) {
    parentFrame->GetAbsoluteContainingBlock()->
      RemoveFrame(parentFrame, aListID, aOldFrame);
  } else {
    rv = parentFrame->RemoveFrame(aListID, aOldFrame);
  }

  mIsDestroyingFrames = wasDestroyingFrames;

  return rv;
}

//----------------------------------------------------------------------

void
nsFrameManager::NotifyDestroyingFrame(nsIFrame* aFrame)
{
  nsIContent* content = aFrame->GetContent();
  if (content && content->GetPrimaryFrame() == aFrame) {
    ClearAllUndisplayedContentIn(content);
  }
}

// Capture state for a given frame.
// Accept a content id here, in some cases we may not have content (scroll position)
void
nsFrameManager::CaptureFrameStateFor(nsIFrame* aFrame,
                                     nsILayoutHistoryState* aState)
{
  if (!aFrame || !aState) {
    NS_WARNING("null frame, or state");
    return;
  }

  // Only capture state for stateful frames
  nsIStatefulFrame* statefulFrame = do_QueryFrame(aFrame);
  if (!statefulFrame) {
    return;
  }

  // Capture the state, exit early if we get null (nothing to save)
  nsAutoPtr<nsPresState> frameState;
  nsresult rv = statefulFrame->SaveState(getter_Transfers(frameState));
  if (!frameState) {
    return;
  }

  // Generate the hash key to store the state under
  // Exit early if we get empty key
  nsAutoCString stateKey;
  nsIContent* content = aFrame->GetContent();
  nsIDocument* doc = content ? content->GetCurrentDoc() : nullptr;
  rv = nsContentUtils::GenerateStateKey(content, doc, stateKey);
  if(NS_FAILED(rv) || stateKey.IsEmpty()) {
    return;
  }

  // Store the state. aState owns frameState now.
  aState->AddState(stateKey, frameState.forget());
}

void
nsFrameManager::CaptureFrameState(nsIFrame* aFrame,
                                  nsILayoutHistoryState* aState)
{
  NS_PRECONDITION(nullptr != aFrame && nullptr != aState, "null parameters passed in");

  CaptureFrameStateFor(aFrame, aState);

  // Now capture state recursively for the frame hierarchy rooted at aFrame
  nsIFrame::ChildListIterator lists(aFrame);
  for (; !lists.IsDone(); lists.Next()) {
    nsFrameList::Enumerator childFrames(lists.CurrentList());
    for (; !childFrames.AtEnd(); childFrames.Next()) {
      nsIFrame* child = childFrames.get();
      if (child->GetStateBits() & NS_FRAME_OUT_OF_FLOW) {
        // We'll pick it up when we get to its placeholder
        continue;
      }
      // Make sure to walk through placeholders as needed, so that we
      // save state for out-of-flows which may not be our descendants
      // themselves but whose placeholders are our descendants.
      CaptureFrameState(nsPlaceholderFrame::GetRealFrameFor(child), aState);
    }
  }
}

// Restore state for a given frame.
// Accept a content id here, in some cases we may not have content (scroll position)
void
nsFrameManager::RestoreFrameStateFor(nsIFrame* aFrame,
                                     nsILayoutHistoryState* aState)
{
  if (!aFrame || !aState) {
    NS_WARNING("null frame or state");
    return;
  }

  // Only restore state for stateful frames
  nsIStatefulFrame* statefulFrame = do_QueryFrame(aFrame);
  if (!statefulFrame) {
    return;
  }

  // Generate the hash key the state was stored under
  // Exit early if we get empty key
  nsIContent* content = aFrame->GetContent();
  // If we don't have content, we can't generate a hash
  // key and there's probably no state information for us.
  if (!content) {
    return;
  }

  nsAutoCString stateKey;
  nsIDocument* doc = content->GetCurrentDoc();
  nsresult rv = nsContentUtils::GenerateStateKey(content, doc, stateKey);
  if (NS_FAILED(rv) || stateKey.IsEmpty()) {
    return;
  }

  // Get the state from the hash
  nsPresState* frameState = aState->GetState(stateKey);
  if (!frameState) {
    return;
  }

  // Restore it
  rv = statefulFrame->RestoreState(frameState);
  if (NS_FAILED(rv)) {
    return;
  }

  // If we restore ok, remove the state from the state table
  aState->RemoveState(stateKey);
}

void
nsFrameManager::RestoreFrameState(nsIFrame* aFrame,
                                  nsILayoutHistoryState* aState)
{
  NS_PRECONDITION(nullptr != aFrame && nullptr != aState, "null parameters passed in");
  
  RestoreFrameStateFor(aFrame, aState);

  // Now restore state recursively for the frame hierarchy rooted at aFrame
  nsIFrame::ChildListIterator lists(aFrame);
  for (; !lists.IsDone(); lists.Next()) {
    nsFrameList::Enumerator childFrames(lists.CurrentList());
    for (; !childFrames.AtEnd(); childFrames.Next()) {
      RestoreFrameState(childFrames.get(), aState);
    }
  }
}

//----------------------------------------------------------------------

static PLHashNumber
HashKey(void* key)
{
  return NS_PTR_TO_INT32(key);
}

static int
CompareKeys(void* key1, void* key2)
{
  return key1 == key2;
}

//----------------------------------------------------------------------

nsFrameManagerBase::UndisplayedMap::UndisplayedMap(uint32_t aNumBuckets)
{
  MOZ_COUNT_CTOR(nsFrameManagerBase::UndisplayedMap);
  mTable = PL_NewHashTable(aNumBuckets, (PLHashFunction)HashKey,
                           (PLHashComparator)CompareKeys,
                           (PLHashComparator)nullptr,
                           nullptr, nullptr);
  mLastLookup = nullptr;
}

nsFrameManagerBase::UndisplayedMap::~UndisplayedMap(void)
{
  MOZ_COUNT_DTOR(nsFrameManagerBase::UndisplayedMap);
  Clear();
  PL_HashTableDestroy(mTable);
}

PLHashEntry**  
nsFrameManagerBase::UndisplayedMap::GetEntryFor(nsIContent** aParentContent)
{
  nsIContent* parentContent = *aParentContent;

  if (mLastLookup && (parentContent == (*mLastLookup)->key)) {
    return mLastLookup;
  }

  // In the case of XBL default content, <xbl:children> elements do not get a
  // frame causing a mismatch between the content tree and the frame tree.
  // |GetEntryFor| is sometimes called with the content tree parent (which may
  // be a <xbl:children> element) but the parent in the frame tree would be the
  // insertion parent (parent of the <xbl:children> element). Here the children
  // elements are normalized to the insertion parent to correct for the mismatch.
  if (parentContent && parentContent->IsActiveChildrenElement()) {
    parentContent = parentContent->GetParent();
    // Change the caller's pointer for the parent content to be the insertion parent.
    *aParentContent = parentContent;
  }

  PLHashNumber hashCode = NS_PTR_TO_INT32(parentContent);
  PLHashEntry** entry = PL_HashTableRawLookup(mTable, hashCode, parentContent);
  if (*entry) {
    mLastLookup = entry;
  }
  return entry;
}

UndisplayedNode* 
nsFrameManagerBase::UndisplayedMap::GetFirstNode(nsIContent* aParentContent)
{
  PLHashEntry** entry = GetEntryFor(&aParentContent);
  if (*entry) {
    return (UndisplayedNode*)((*entry)->value);
  }
  return nullptr;
}

void
nsFrameManagerBase::UndisplayedMap::AppendNodeFor(UndisplayedNode* aNode,
                                                  nsIContent* aParentContent)
{
  PLHashEntry** entry = GetEntryFor(&aParentContent);
  if (*entry) {
    UndisplayedNode*  node = (UndisplayedNode*)((*entry)->value);
    while (node->mNext) {
      if (node->mContent == aNode->mContent) {
        // We actually need to check this in optimized builds because
        // there are some callers that do this.  See bug 118014, bug
        // 136704, etc.
        NS_NOTREACHED("node in map twice");
        delete aNode;
        return;
      }
      node = node->mNext;
    }
    node->mNext = aNode;
  }
  else {
    PLHashNumber hashCode = NS_PTR_TO_INT32(aParentContent);
    PL_HashTableRawAdd(mTable, entry, hashCode, aParentContent, aNode);
    mLastLookup = nullptr; // hashtable may have shifted bucket out from under us
  }
}

nsresult 
nsFrameManagerBase::UndisplayedMap::AddNodeFor(nsIContent* aParentContent,
                                               nsIContent* aChild, 
                                               nsStyleContext* aStyle)
{
  UndisplayedNode*  node = new UndisplayedNode(aChild, aStyle);

  AppendNodeFor(node, aParentContent);
  return NS_OK;
}

void
nsFrameManagerBase::UndisplayedMap::RemoveNodeFor(nsIContent* aParentContent,
                                                  UndisplayedNode* aNode)
{
  PLHashEntry** entry = GetEntryFor(&aParentContent);
  NS_ASSERTION(*entry, "content not in map");
  if (*entry) {
    if ((UndisplayedNode*)((*entry)->value) == aNode) {  // first node
      if (aNode->mNext) {
        (*entry)->value = aNode->mNext;
        aNode->mNext = nullptr;
      }
      else {
        PL_HashTableRawRemove(mTable, entry, *entry);
        mLastLookup = nullptr; // hashtable may have shifted bucket out from under us
      }
    }
    else {
      UndisplayedNode*  node = (UndisplayedNode*)((*entry)->value);
      while (node->mNext) {
        if (node->mNext == aNode) {
          node->mNext = aNode->mNext;
          aNode->mNext = nullptr;
          break;
        }
        node = node->mNext;
      }
    }
  }
  delete aNode;
}

void
nsFrameManagerBase::UndisplayedMap::RemoveNodesFor(nsIContent* aParentContent)
{
  PLHashEntry** entry = GetEntryFor(&aParentContent);
  NS_ASSERTION(entry, "content not in map");
  if (*entry) {
    UndisplayedNode*  node = (UndisplayedNode*)((*entry)->value);
    NS_ASSERTION(node, "null node for non-null entry in UndisplayedMap");
    delete node;
    PL_HashTableRawRemove(mTable, entry, *entry);
    mLastLookup = nullptr; // hashtable may have shifted bucket out from under us
  }
}

static int
RemoveUndisplayedEntry(PLHashEntry* he, int i, void* arg)
{
  UndisplayedNode*  node = (UndisplayedNode*)(he->value);
  delete node;
  // Remove and free this entry and continue enumerating
  return HT_ENUMERATE_REMOVE | HT_ENUMERATE_NEXT;
}

void
nsFrameManagerBase::UndisplayedMap::Clear(void)
{
  mLastLookup = nullptr;
  PL_HashTableEnumerateEntries(mTable, RemoveUndisplayedEntry, 0);
}

uint32_t nsFrameManagerBase::sGlobalGenerationNumber;
