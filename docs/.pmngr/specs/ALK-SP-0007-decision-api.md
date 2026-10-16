---
id: ALK-SP-0007
type: spec
title: Decision API
status: backlog
labels: [decision, api]
created: 2026-10-16T22:37:37Z
updated: 2026-10-16T22:37:37Z
requirements:
  R1:
    status: backlog
  R2:
    status: backlog
  R3:
    status: backlog
  R4:
    status: backlog
  R5:
    status: backlog
---

## Purpose

Serve decision models that answer structured questions with probabilities in one pass, without text generation, compatible with Ollama `/v1/systemone` and LocalAI `/v1/decisions`.

## Scope

Covers request and response shapes, question types, encodings, scoring and model detection. To be confirmed by the product owner as the 'Jev' API.

Plan: [[plan/alkahest-v1-plan]]

## Requirements

### ALK-SP-0007.R1 — System One endpoint

The server SHALL implement `POST /v1/systemone` accepting `{model, state, images?, questions, keep_alive?}` and returning `{model, answers, usage}` as Ollama >= 0.35 does.

#### Scenario: ollama parity
- **WHEN** the same request is sent to Ollama 0.35 and Alkahest with the same model
- **THEN** answers match in shape and chosen values

### ALK-SP-0007.R2 — Question types

The system SHALL support `noul` (yes/no, default), `choice` (label to description map) and `score` (2 to 10 ordered levels), 1 to 64 questions per request, each answer carrying probabilities and confidence.

#### Scenario: too many questions
- **WHEN** 65 questions are sent
- **THEN** 400

### ALK-SP-0007.R3 — Logit scoring

Answers SHALL be computed from the engine's token log-probabilities over the candidate options, never by sampling text.

#### Scenario: deterministic
- **WHEN** the same request runs twice
- **THEN** probabilities are identical

### ALK-SP-0007.R4 — Encodings

The system SHALL compile requests with the tev1, kev, clef or strands encoding chosen from the model metadata (`decision.type` or renderer).

#### Scenario: unknown encoding
- **WHEN** a non-decision model is used
- **THEN** 400 explaining the model is not a decision model

### ALK-SP-0007.R5 — Decisions alias

The server SHALL implement `POST /v1/decisions` with the LocalAI OpenAI-style shape mapped onto the same engine.

#### Scenario: alias
- **WHEN** a LocalAI client calls `/v1/decisions`
- **THEN** it gets a valid response
