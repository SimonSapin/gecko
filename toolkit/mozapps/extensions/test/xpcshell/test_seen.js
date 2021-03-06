/* Any copyright is dedicated to the Public Domain.
 * http://creativecommons.org/publicdomain/zero/1.0/
 */

const ID = "bootstrap1@tests.mozilla.org";

let profileDir = gProfD.clone();
profileDir.append("extensions");

createAppInfo("xpcshell@tests.mozilla.org", "XPCShell", "1", "1.9.2");
startupManager();

// By default disable add-ons from the profile
Services.prefs.setIntPref("extensions.autoDisableScopes", AddonManager.SCOPE_PROFILE);

// Installing an add-on through the API should mark it as seen
add_task(async function() {
  let install = await promiseInstallFile(do_get_addon("test_bootstrap1_1"));
  do_check_eq(install.state, AddonManager.STATE_INSTALLED);
  do_check_false(hasFlag(install.addon.pendingOperations, AddonManager.PENDING_INSTALL));

  let addon = install.addon;
  do_check_eq(addon.version, "1.0");
  do_check_false(addon.foreignInstall);
  do_check_true(addon.seen);

  await promiseRestartManager();

  addon = await promiseAddonByID(ID);
  do_check_false(addon.foreignInstall);
  do_check_true(addon.seen);

  // Installing an update should retain that
  install = await promiseInstallFile(do_get_addon("test_bootstrap1_2"));
  do_check_eq(install.state, AddonManager.STATE_INSTALLED);
  do_check_false(hasFlag(install.addon.pendingOperations, AddonManager.PENDING_INSTALL));

  addon = install.addon;
  do_check_eq(addon.version, "2.0");
  do_check_false(addon.foreignInstall);
  do_check_true(addon.seen);

  await promiseRestartManager();

  addon = await promiseAddonByID(ID);
  do_check_false(addon.foreignInstall);
  do_check_true(addon.seen);

  addon.uninstall();
  await promiseShutdownManager();
});

// Sideloading an add-on should mark it as unseen
add_task(async function() {
  let path = manuallyInstall(do_get_addon("test_bootstrap1_1"), profileDir, ID);
  // Make sure the startup code will detect sideloaded updates
  setExtensionModifiedTime(path, Date.now() - 10000);

  startupManager();

  let addon = await promiseAddonByID(ID);
  do_check_eq(addon.version, "1.0");
  do_check_true(addon.foreignInstall);
  do_check_false(addon.seen);

  await promiseRestartManager();

  addon = await promiseAddonByID(ID);
  do_check_true(addon.foreignInstall);
  do_check_false(addon.seen);

  await promiseShutdownManager();

  // Sideloading an update shouldn't change the state
  manuallyUninstall(profileDir, ID);
  manuallyInstall(do_get_addon("test_bootstrap1_2"), profileDir, ID);
  setExtensionModifiedTime(path, Date.now());

  startupManager();

  addon = await promiseAddonByID(ID);
  do_check_eq(addon.version, "2.0");
  do_check_true(addon.foreignInstall);
  do_check_false(addon.seen);

  addon.uninstall();
  await promiseShutdownManager();
});

// Sideloading an add-on should mark it as unseen
add_task(async function() {
  let path = manuallyInstall(do_get_addon("test_bootstrap1_1"), profileDir, ID);
  // Make sure the startup code will detect sideloaded updates
  setExtensionModifiedTime(path, Date.now() - 10000);

  startupManager();

  let addon = await promiseAddonByID(ID);
  do_check_eq(addon.version, "1.0");
  do_check_true(addon.foreignInstall);
  do_check_false(addon.seen);

  await promiseRestartManager();

  addon = await promiseAddonByID(ID);
  do_check_true(addon.foreignInstall);
  do_check_false(addon.seen);

  // Updating through the API shouldn't change the state
  let install = await promiseInstallFile(do_get_addon("test_bootstrap1_2"));
  do_check_eq(install.state, AddonManager.STATE_INSTALLED);
  do_check_false(hasFlag(install.addon.pendingOperations, AddonManager.PENDING_INSTALL));

  addon = install.addon;
  do_check_true(addon.foreignInstall);
  do_check_false(addon.seen);

  await promiseRestartManager();

  addon = await promiseAddonByID(ID);
  do_check_eq(addon.version, "2.0");
  do_check_true(addon.foreignInstall);
  do_check_false(addon.seen);

  addon.uninstall();
  await promiseShutdownManager();
});

// Sideloading an add-on should mark it as unseen
add_task(async function() {
  let path = manuallyInstall(do_get_addon("test_bootstrap1_1"), profileDir, ID);
  // Make sure the startup code will detect sideloaded updates
  setExtensionModifiedTime(path, Date.now() - 10000);

  startupManager();

  let addon = await promiseAddonByID(ID);
  do_check_eq(addon.version, "1.0");
  do_check_true(addon.foreignInstall);
  do_check_false(addon.seen);
  addon.markAsSeen();
  do_check_true(addon.seen);

  await promiseRestartManager();

  addon = await promiseAddonByID(ID);
  do_check_true(addon.foreignInstall);
  do_check_true(addon.seen);

  await promiseShutdownManager();

  // Sideloading an update shouldn't change the state
  manuallyUninstall(profileDir, ID);
  manuallyInstall(do_get_addon("test_bootstrap1_2"), profileDir, ID);
  setExtensionModifiedTime(path, Date.now());

  startupManager();

  addon = await promiseAddonByID(ID);
  do_check_eq(addon.version, "2.0");
  do_check_true(addon.foreignInstall);
  do_check_true(addon.seen);

  addon.uninstall();
  await promiseShutdownManager();
});

// Sideloading an add-on should mark it as unseen
add_task(async function() {
  let path = manuallyInstall(do_get_addon("test_bootstrap1_1"), profileDir, ID);
  // Make sure the startup code will detect sideloaded updates
  setExtensionModifiedTime(path, Date.now() - 10000);

  startupManager();

  let addon = await promiseAddonByID(ID);
  do_check_eq(addon.version, "1.0");
  do_check_true(addon.foreignInstall);
  do_check_false(addon.seen);
  addon.markAsSeen();
  do_check_true(addon.seen);

  await promiseRestartManager();

  addon = await promiseAddonByID(ID);
  do_check_true(addon.foreignInstall);
  do_check_true(addon.seen);

  // Updating through the API shouldn't change the state
  let install = await promiseInstallFile(do_get_addon("test_bootstrap1_2"));
  do_check_eq(install.state, AddonManager.STATE_INSTALLED);
  do_check_false(hasFlag(install.addon.pendingOperations, AddonManager.PENDING_INSTALL));

  addon = install.addon;
  do_check_true(addon.foreignInstall);
  do_check_true(addon.seen);

  await promiseRestartManager();

  addon = await promiseAddonByID(ID);
  do_check_eq(addon.version, "2.0");
  do_check_true(addon.foreignInstall);
  do_check_true(addon.seen);

  addon.uninstall();
  await promiseShutdownManager();
});
