// Copyright (C) 2015 André Bargull. All rights reserved.
// This code is governed by the BSD license found in the LICENSE file.

/*---
es6id: 22.2.5.1
description: >
  The initial value of Int8Array.BYTES_PER_ELEMENT is 1.
info: >
  The value of TypedArray.BYTES_PER_ELEMENT is the Number value of the
  Element Size value specified in Table 49 for TypedArray.

  This property has the attributes { [[Writable]]: false, [[Enumerable]]: false, [[Configurable]]: false }.
includes: [propertyHelper.js]
---*/

assert.sameValue(Int8Array.BYTES_PER_ELEMENT, 1);

verifyNotEnumerable(Int8Array, "BYTES_PER_ELEMENT");
verifyNotWritable(Int8Array, "BYTES_PER_ELEMENT");
verifyNotConfigurable(Int8Array, "BYTES_PER_ELEMENT");

reportCompare(0, 0);
