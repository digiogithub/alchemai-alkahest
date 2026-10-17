# Model detail `default_parameters` and per-model parallel slots

Date: 2026-10-17. Stories [[ALK-US-0103]] (bug) and [[ALK-US-0102]]; requirements
[[ALK-SP-0005.R6]], [[ALK-SP-0010.R4]], [[ALK-SP-0008.R5]]. Builds on
[[2026-10-17-admin-api]], [[2026-10-17-model-config-overlay]] and
[[2026-10-17-desktop-model-settings]]. Plan: [[plan/alkahest-v1-plan]].

## What changed

### ALK-US-0103: one JSON type per key in the model detail

- `alkahest-api` `admin/models.rs#detail`: the params layer's default map is sent as
  `default_parameters`; `parameters` stays the row's parameter-count label (`"494M"`), so the
  detail route is a strict superset of a catalog row. Route summary updated in the OpenAPI
  table (`admin/mod.rs`).
- `alkahest-client` `admin::CatalogModel::default_parameters: Map<String, Value>`. The lenient
  `label_or_none` reader stays for servers from before the fix.
- `alkahest-desktop`: `model_settings::default_parameters_line` (Modelfile-like
  `stop "<|im_end|>" · temperature 0.7`); the chat template card of screen 12 shows it as
  "Model file parameters".

### ALK-US-0102: `num_parallel` in the model overlay

- `alkahest-store` `overlay::ModelSettings::num_parallel: Option<u32>` (also in the field
  macro, so it merges and reports its origin like every field), `MAX_NUM_PARALLEL = 64`;
  `set_key` / `unset_key` accept `num_parallel` and `parallel` (`alkahest config model <m>
  set num_parallel=4`), refuse 0 and > 64. `global_defaults` carries
  `scheduler.num_parallel` when it is not automatic, so the editor shows the global value with
  its origin.
- `alkahest-api` admin settings `PUT` validates 1..=64; the memory estimate uses the
  effective per-model value when the query does not override it.
- `alkahest-sched` `scheduler::parallel_slots(model, opts)`: the model's effective value, else
  `scheduler.num_parallel`. Used for the launch request (`-np` / `--max-num-seqs`), the load
  fingerprint (a change reloads the model when idle), the slot semaphore, the queue report,
  the status (`num_parallel`) and the two admission estimates. The scheduler edit is limited to
  those call sites (another stream edits the memory/admission internals).
- `alkahest-desktop` screen 12: "Parallel slots" is a saved overlay field (`FIELDS`,
  `SettingsForm::parallel()`), no longer "estimate only".

## Why

ALK-US-0103: `Client::model()` failed to decode every detail answer before the workaround,
and the default parameter map was lost to client users. ALK-US-0102: ALK-SP-0008.R5 lists
parallel slots among the per-model settings, and a decision model that serves many short
requests wants more slots than a large chat model on the same server.

## Verification

- Tests: `crates/alkahest-api/tests/admin_api.rs#catalog_lists_kind_source_engine_context_paths_and_runtime`
  (label and map under separate keys), `crates/alkahest-client/src/admin.rs#model_detail_keeps_the_label_and_the_default_parameters`,
  `crates/alkahest-store/src/overlay.rs#num_parallel_is_a_model_setting_with_limits`,
  `crates/alkahest-sched/tests/scheduler.rs#per_model_parallel_slots_override_the_scheduler_value`
  (three concurrent leases on one engine with `num_parallel = 3` over a global 2; launch
  request, queue slots), `crates/alkahest-desktop/src/model_settings.rs#launch_line_and_query_follow_the_draft`,
  `#default_parameters_read_like_a_modelfile`.
- `cargo nextest run` for alkahest-store, -sched, -api, -client, -desktop, alkahest: 573
  passed; desktop GUI tests (`--features gui-test views::`) passed; clippy `-D warnings` and
  fmt clean for the touched crates. Full `scripts/ci-local.sh --desktop` ran on the final
  branch state (see [[2026-10-17-desktop-keys-engines-settings]]).
