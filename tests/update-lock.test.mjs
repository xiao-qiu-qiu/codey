import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const read = (path) => readFile(new URL(`../${path}`, import.meta.url), "utf8");

test("local builds lock every online update entry point", async () => {
  const [config, startup, updates, hook] = await Promise.all([
    read("backend/src/config.rs"),
    read("backend/src/startup_update.rs"),
    read("backend/src/commands/updates.rs"),
    read("src/useAppUpdates.ts"),
  ]);

  assert.match(config, /CODEY_ENABLE_SELF_UPDATE/);
  assert.match(startup, /if !crate::config::self_update_enabled\(\)/);
  assert.match(updates, /ensure_self_update_enabled\(\)\?;/);
  assert.match(updates, /self_update_enabled: false/);
  assert.match(hook, /if \(!result\.selfUpdateEnabled\)/);
  assert.match(hook, /let updatesLocked = false/);
});
