---
id: ALK-SP-0007
type: spec
title: Decision API
status: backlog
labels: [decision, api]
created: 2026-10-16T22:37:37Z
updated: 2026-10-17T01:45:50Z
requirements:
  R1:
    status: backlog
  R2:
    status: backlog
    trace:
      code:
        - crates/alkahest-decision/src/types.rs#SystemOneRequest
        - crates/alkahest-decision/src/types.rs#QuestionType
        - crates/alkahest-decision/src/compile.rs#validate_limits
        - crates/alkahest-decision/src/compile.rs#compile
        - crates/alkahest-decision/src/compile.rs#Compiled::answer
      tests:
        - crates/alkahest-decision/tests/compile_golden.rs#too_many_questions
        - crates/alkahest-decision/tests/compile_golden.rs#score_levels_two_to_ten_for_every_encoding
        - crates/alkahest-decision/tests/compile_golden.rs#invalid_requests_are_400
        - crates/alkahest-decision/tests/compile_golden.rs#answers_softmax_ties_and_usage
        - crates/alkahest-decision/tests/compile_golden.rs#missing_type_defaults_to_noul
  R3:
    status: backlog
    trace:
      code:
        - crates/alkahest-decision/src/scoring.rs#DecisionEngine
        - crates/alkahest-decision/src/scoring.rs#ScoringOptions
        - crates/alkahest-engine/src/llama_cpp.rs#LlamaCppEngine::model_args
      tests:
        - crates/alkahest-decision/tests/scoring_fake.rs#deterministic
        - crates/alkahest-decision/tests/scoring_fake.rs#probabilities_are_the_softmax_over_candidate_logits
        - crates/alkahest-decision/tests/scoring_fake.rs#outranked_candidates_are_retried_with_a_larger_bias
        - crates/alkahest-decision/tests/scoring_real.rs#real_tev1_decisions_are_deterministic
        - crates/alkahest-engine/tests/llama_cpp.rs#decision_kind_gets_slots_unified_kv_and_no_prompt_cache
  R4:
    status: backlog
    trace:
      code:
        - crates/alkahest-decision/src/encoding.rs#Encoding
        - crates/alkahest-decision/src/encoding.rs#DecisionMetadata::plan
        - crates/alkahest-decision/src/compile.rs#compile
      tests:
        - crates/alkahest-decision/src/encoding.rs#tests::unknown_encoding
        - crates/alkahest-decision/src/encoding.rs#tests::renderer_names_the_encoding
        - crates/alkahest-decision/tests/compile_golden.rs#publisher_prompts_tev1_and_generic
        - crates/alkahest-decision/tests/compile_golden.rs#kev_request_and_answers
        - crates/alkahest-decision/tests/compile_golden.rs#clef_joint_prompt_segments
        - crates/alkahest-decision/tests/compile_golden.rs#strands_pointer_rows
  R5:
    status: backlog
---

## Purpose

Serve decision models that answer structured questions with probabilities in one pass, without text generation, compatible with Ollama `/v1/systemone` and LocalAI `/v1/decisions`.

## Scope

Covers request and response shapes, question types, encodings, scoring and model detection. Jev was the first commercial decision-model API; open-source projects (Ollama, LocalAI) cloned its API as `/v1/systemone` so clients can move between services transparently. Alkahest implements the same contract (product owner decision, 2026-10-17).

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
