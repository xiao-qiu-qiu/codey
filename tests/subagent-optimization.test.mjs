import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("subagent optimization exposes per-task model and reasoning controls", async () => {
  const [appSource, modelHookSource, featurePolicySource] = await Promise.all([
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/useModelSelection.ts", root), "utf8"),
    readFile(new URL("src/FeaturePolicyCard.tsx", root), "utf8"),
  ]);
  const uiSource = `${appSource}\n${featurePolicySource}`;

  assert.match(uiSource, /checked=\{config\.subagentOptimization\}/);
  assert.match(
    uiSource,
    /onCheckedChange=\{\(checked\) =>\s*onSubagentOptimizationChange\(checked\)\s*\}/,
  );
  assert.match(uiSource, /Codey 子代理角色与调度增强/);
  assert.match(uiSource, /aria-label="启用 Codey 子代理角色与调度增强"/);
  assert.match(uiSource, /实际受父任务权限模式约束/);
  for (const role of [
    "codey_quick_scan",
    "codey_deep_research",
    "codey_visual_analysis",
    "codey_worker",
    "codey_visual_worker",
    "default",
  ]) {
    assert.match(uiSource, new RegExp(`id: "${role}"`));
  }
  for (const label of [
    "快速定位",
    "深度检索",
    "视觉分析",
    "代码实施",
    "视觉实施",
    "通用兜底",
  ]) {
    assert.match(uiSource, new RegExp(`name: "${label}"`));
  }
  assert.match(uiSource, /config\.subagentOptimization \? \(/);
  assert.match(uiSource, /className="subagent-task-help"/);
  assert.match(uiSource, /content=\{task\.description\}/);
  assert.match(uiSource, /aria-labelledby=\{`\$\{task\.id\}-model-label`\}/);
  assert.match(uiSource, /aria-labelledby=\{`\$\{task\.id\}-effort-label`\}/);
  assert.match(uiSource, /config\.subagentRoles\[task\.id\]/);
  assert.match(uiSource, /\[task\.id\]: \{ model, reasoningEffort \}/);
  assert.match(
    uiSource,
    /subagentPolicyControlsDisabled \|\|\s*subagentModelOptions\.length === 0/,
  );
  assert.match(
    uiSource,
    /subagentPolicyControlsDisabled \|\|\s*reasoningEfforts\.length === 0/,
  );
  assert.match(
    modelHookSource,
    /model\.supported\s*&&\s*model\.supportsSubagent/,
  );
  assert.match(uiSource, /label:\s*option\.label/);
  assert.doesNotMatch(modelHookSource, /roleSchedulingSupported|supportsSubagentV2/);
  assert.doesNotMatch(uiSource, /原生 V1，角色调度不可用/);
  assert.doesNotMatch(modelHookSource, /\.\.\.modelState\.thirdPartyModels\s*\.map/);
  assert.doesNotMatch(modelHookSource, /subagentModelIds|subagentModelKeys/);
  assert.doesNotMatch(
    uiSource,
    /check-subagent-model|当前线路没有 Codex 子代理工具可用的模型/,
  );
  assert.doesNotMatch(uiSource, /仅接受 Sol \/ Terra/);
  for (const [role, name, model, effort] of [
    ["codey_luna", "Luna", "gpt-5.6-luna", "max"],
    ["codey_terra", "Terra", "gpt-5.6-terra", "max"],
    ["codey_sol", "Sol", "gpt-5.6-sol", "xhigh"],
  ]) {
    assert.match(uiSource, new RegExp(`id: "${role}"`));
    assert.match(uiSource, new RegExp(`name: "${name}"`));
    assert.match(uiSource, new RegExp(`model: "${model.replaceAll(".", "\\.")}"`));
    assert.match(uiSource, new RegExp(`reasoningEffort: "${effort}"`));
  }
  assert.match(uiSource, /SUBAGENT_FIXED_ROLE_TYPES\.map/);
  assert.match(uiSource, /<Badge variant="secondary">固定<\/Badge>/);
  assert.doesNotMatch(uiSource, /subagentRoles:\s*\{[^}]*codey_luna/s);
});

test("subagent dispatch guidance is editable in settings and applied at startup", async () => {
  const [typesSource, appSource, featureSource, stylesSource, commandSource, configSource] =
    await Promise.all([
      readFile(new URL("src/App.types.ts", root), "utf8"),
      readFile(new URL("src/App.tsx", root), "utf8"),
      readFile(new URL("src/FeaturePolicyCard.tsx", root), "utf8"),
      readFile(new URL("src/styles.features.css", root), "utf8"),
      readFile(new URL("backend/src/commands.rs", root), "utf8"),
      readFile(new URL("backend/src/codex_config.rs", root), "utf8"),
    ]);

  assert.match(typesSource, /subagentGuidance: string/);
  assert.match(appSource, /defaultSubagentGuidance=\{defaultSubagentGuidance\}/);
  assert.match(featureSource, /id="subagent-guidance"/);
  assert.match(featureSource, /value=\{config\.subagentGuidance\}/);
  assert.match(featureSource, /subagentGuidance: event\.currentTarget\.value/);
  assert.match(featureSource, /subagentGuidance: defaultSubagentGuidance/);
  assert.match(featureSource, /maxLength=\{32768\}/);
  assert.match(featureSource, /恢复默认/);
  assert.match(stylesSource, /\.subagent-guidance-textarea/);
  assert.match(commandSource, /"defaultSubagentGuidance": default_subagent_guidance\(\)/);
  assert.match(commandSource, /validate_subagent_guidance/);
  assert.match(configSource, /subagent_guidance: options\.subagent_guidance/);
  assert.match(configSource, /append_subagent_guidance\(\s*existing_agents_md,\s*subagent_guidance/);
  assert.match(configSource, /write_managed_constraint_file\(&root_path, subagent_guidance\)/);
});

test("leaf subagent models do not inherit coordinator capability markers", async () => {
  const catalogSource = await readFile(
    new URL("backend/src/model_catalog.rs", root),
    "utf8",
  );

  assert.doesNotMatch(catalogSource, /enable_subagents_for_all_models/);
  assert.match(catalogSource, /object\.remove\("multi_agent_version"\)/);
  assert.match(
    catalogSource,
    /generated_catalog_preserves_official_multi_agent_markers/,
  );
  assert.match(
    catalogSource,
    /generated_catalog_keeps_leaf_models_without_v2_coordinator_markers/,
  );
});

test("subagent guidance puts the complete assignment in the spawn request", async () => {
  const guidanceSource = await readFile(
    new URL("backend/src/codex_config_guidance.rs", root),
    "utf8",
  );

  assert.match(guidanceSource, /初始任务字段/);
  assert.match(guidanceSource, /`message` 或 `task`/);
  assert.match(guidanceSource, /`task_name`、角色名、模型名和 `fork_turns` 都只是元数据/);
  assert.match(guidanceSource, /角色参数只有在当前工具 schema 明确声明时才传入/);
  assert.match(guidanceSource, /These agent tools are not in the/);
  assert.match(guidanceSource, /`functions` namespace/);
  assert.match(guidanceSource, /`functions\.spawn_agent`/);
  assert.match(guidanceSource, /The canonical dispatch/);
  assert.match(guidanceSource, /Correcting agent tool usage/);
  assert.match(guidanceSource, /`agents\.followup_task` 补发同一份完整、自包含的任务正文/);
});

test("per-task subagent files are composed at startup and hot-refreshed after save", async () => {
  const [commandSource, configSource, launcherSource, rendererSource, vendorRendererSource] = await Promise.all([
    readFile(new URL("backend/src/commands.rs", root), "utf8"),
    readFile(new URL("backend/src/codex_config.rs", root), "utf8"),
    readFile(new URL("backend/src/launcher.rs", root), "utf8"),
    readFile(new URL("public/renderer-inject.js", root), "utf8"),
    readFile(new URL("vendor/CodeyRuntime/assets/inject/renderer-inject.js", root), "utf8"),
  ]);

  assert.match(commandSource, /hot_reload_runtime_subagent_config/);
  assert.match(commandSource, /refresh_runtime_subagent_roles/);
  assert.doesNotMatch(commandSource, /let refresh_subagent_defaults = false/);
  assert.match(configSource, /fn prepare_runtime_agent_files/);
  assert.match(configSource, /document\["model"\] = value\(model\)/);
  assert.match(
    configSource,
    /document\["model_reasoning_effort"\] = value\(&reasoning_effort\)/,
  );
  assert.match(configSource, /register_runtime_agents/);
  assert.match(configSource, /agents\.\{\}\.config_file/);
  assert.match(configSource, /pub fn refresh_runtime_subagent_roles/);
  assert.match(configSource, /verify_runtime_agent_files/);
  assert.match(configSource, /restore_runtime_agent_files_and_lease/);
  assert.match(configSource, /SUBAGENT_RUNTIME_ROLE_IDS/);
  assert.match(configSource, /fixed_subagent_role_config/);
  assert.match(launcherSource, /subagent_roles: Some\(&subagent_roles\)/);
  assert.match(launcherSource, /supports_subagent_config_hot_reload/);
  assert.doesNotMatch(rendererSource, /__codeyApplySubagentDefaults/);
  assert.doesNotMatch(vendorRendererSource, /__codeyApplySubagentDefaults/);
  assert.doesNotMatch(vendorRendererSource, /patchAppServerSubagentRequestParams/);
});

test("subagent optimization keeps native agents and removes legacy gate hooks", async () => {
  const [gateSource, configSource, mainSource] = await Promise.all([
    readFile(new URL("backend/src/subagent_gate.rs", root), "utf8"),
    readFile(new URL("backend/src/codex_config.rs", root), "utf8"),
    readFile(new URL("backend/src/main.rs", root), "utf8"),
  ]);

  assert.match(mainSource, /run_subagent_gate_hook_if_requested/);
  assert.match(configSource, /remove_subagent_gate_hooks\(doc, config_path\)/);
  assert.doesNotMatch(configSource, /enable_subagent_gate_hooks\(/);
  assert.match(configSource, /json_contains_subagent_gate_hooks/);
  assert.match(configSource, /FASTCTX_ROUTE_HOOKS/);
  assert.match(gateSource, /cleanup_stale_state/);
  assert.match(gateSource, /STATE_DIRECTORY/);
  assert.match(configSource, /trusted_hash/);
  assert.match(gateSource, /nonempty\(input\.agent_id\.as_deref\(\)\)\.is_some\(\)/);
  assert.match(gateSource, /permissionDecision": "deny"/);
  assert.match(gateSource, /"decision": "block"/);
  assert.match(gateSource, /is_collaboration_tool/);
  assert.match(gateSource, /is_spawn_agent_tool/);
  assert.match(gateSource, /subagent_spawn_denial/);
  assert.match(gateSource, /子代理不能继续派生子代理/);
  assert.match(gateSource, /post_wait_continuation/);
  assert.match(gateSource, /可读取它并仅使用 agents\.send_message/);
  assert.match(gateSource, /不得恢复非协作本地工作/);
  assert.match(gateSource, /SubagentStop/);
});
