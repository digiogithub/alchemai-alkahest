---
id: ALK-US-0103
type: story
title: Admin model detail reuses the `parameters` key with a different type
status: in_progress
priority: medium
parent: ALK-EP-0007
author: mcp
labels: [api, bug]
created: 2026-10-17T04:30:45Z
updated: 2026-10-17T04:42:31Z
started: 2026-10-17T04:42:31Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T04:30:45Z
---

## Bug

`GET /alkahest/v1/models/{name}` builds the catalog row (where `parameters` is the parameter-count label, a string like `"494M"`) and then overwrites `v["parameters"]` with the model's default parameter map (`ModelDetail::parameters`, an object). `alkahest_client::admin::CatalogModel::parameters: Option<String>` therefore failed to decode every detail answer, so `Client::model()` always returned a decode error (seen in the desktop model settings: no template shown).

## Workaround in place

ALK-US-0069 made the client lenient (`label_or_none`: a non-string `parameters` reads as `None`; test `admin::tests::model_detail_with_a_parameter_map_decodes`). The default parameter map is still lost to client users.

## Fix proposal

In `crates/alkahest-api/src/admin/models.rs#detail`, emit the map under its own key (`default_parameters` or `modelfile_parameters`) and keep `parameters` as the label; add `CatalogModel` doc / an accessor for the map. Update the OpenAPI table.
