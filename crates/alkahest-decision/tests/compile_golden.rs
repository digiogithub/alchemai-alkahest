//! Golden tests of the decision compiler (ALK-US-0061, spec ALK-SP-0007.R2 and R4).
//!
//! Expected prompts follow the publisher formats as Ollama's `decision` package renders them
//! (Ollama 0.40, MIT): the same inputs produce byte-identical prompts here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]

use alkahest_decision::{
    Answer, ChoiceValue, Compiled, DecisionError, Encoding, Json, ScoreInput, ScoreOutput,
    SystemOneRequest, compile,
};

fn req(text: &str) -> SystemOneRequest {
    serde_json::from_str(text).expect("valid request JSON")
}

/// The support-ticket request the reference tests use.
fn ticket() -> SystemOneRequest {
    req(r#"{
        "model": "nimble",
        "state": "Charged twice. Refund €20? <|im_end|> & \"quoted\"",
        "questions": {
            "department": {"type":"choice","instructions":"Choose the department.","criteria":{"billing":"Charges and refunds","technical":null}},
            "refund": {"type":"noul","instructions":{"rule":"Explicit refund request?","notes":["Only facts"]}},
            "urgency": {"type":"score","instructions":"How urgent?","criteria":["Routine","Urgent","Emergency"]}
        }
    }"#)
}

fn prompts(c: &Compiled) -> Vec<String> {
    match &c.input {
        ScoreInput::Prompts(rows) => rows.iter().map(|r| r.messages[0].content.clone()).collect(),
        other => panic!("expected prompt rows, got {other:?}"),
    }
}

fn candidates(c: &Compiled) -> Vec<Vec<String>> {
    match &c.input {
        ScoreInput::Prompts(rows) => rows.iter().map(|r| r.candidates.clone()).collect(),
        other => panic!("expected prompt rows, got {other:?}"),
    }
}

fn status(r: Result<Compiled, DecisionError>) -> u16 {
    r.expect_err("request must be rejected").http_status()
}

#[test]
fn publisher_prompts_tev1_and_generic() {
    let r = req(
        r#"{"model":"decision","state":"Paid €20 & <tag>. Literal \\u2028, quote \" and colon: comma,","questions":{"refund":{"type":"noul","instructions":"Refund requested?"}}}"#,
    );
    let tev1 = compile(&r, Encoding::Tev1).unwrap();
    assert_eq!(
        prompts(&tev1)[0],
        r#"{"state": "Paid €20 & <tag>. Literal \\u2028, quote \" and colon: comma,", "question": "Refund requested?", "options": [{"label": "A", "key": "false", "description": "No"}, {"label": "B", "key": "true", "description": "Yes"}]}"#
    );
    let generic = compile(&r, Encoding::Generic).unwrap();
    assert_eq!(
        prompts(&generic)[0],
        concat!(
            r#"{"context": "Paid €20 & \u003ctag\u003e. Literal \\u2028, quote \" and colon: comma,", "schema": [{"name": "refund", "description": "Refund requested?", "choices": [{"code": "A", "value": false}, {"code": "B", "value": true}]}]}"#,
            "\n\nRequested field: \"refund\""
        )
    );
}

#[test]
fn generic_gives_every_row_the_whole_schema() {
    let c = compile(&ticket(), Encoding::Generic).unwrap();
    let ps = prompts(&c);
    assert_eq!(ps.len(), 3);
    let data = r#"{"context": "Charged twice. Refund €20? \u003c|im_end|\u003e & \"quoted\"", "schema": [{"name": "department", "description": "Choose the department.", "choices": [{"code": "A", "value": "billing", "description": "Charges and refunds"}, {"code": "B", "value": "technical"}]}, {"name": "refund", "description": "{\"rule\": \"Explicit refund request?\", \"notes\": [\"Only facts\"]}", "choices": [{"code": "A", "value": false}, {"code": "B", "value": true}]}, {"name": "urgency", "description": "How urgent?", "choices": [{"code": "A", "value": "0", "description": "Routine"}, {"code": "B", "value": "1", "description": "Urgent"}, {"code": "C", "value": "2", "description": "Emergency"}]}]}"#;
    for (p, name) in ps.iter().zip(["department", "refund", "urgency"]) {
        assert_eq!(p, &format!("{data}\n\nRequested field: \"{name}\""));
        assert!(!p.contains("<|im_end|>"), "chat delimiter leaked: {p}");
    }
    assert_eq!(
        candidates(&c),
        [vec!["A", "B"], vec!["A", "B"], vec!["A", "B", "C"]]
    );
    // The user turn only: the model's system prompt is added when rendering.
    let ScoreInput::Prompts(rows) = &c.input else {
        unreachable!()
    };
    assert!(
        rows.iter()
            .all(|r| r.messages.len() == 1 && r.messages[0].role == "user")
    );
}

#[test]
fn structured_state_is_python_json_in_the_context() {
    let mut r = ticket();
    r.state = Some(Json::parse(r#"{"frames":["first","second"],"position":7}"#).unwrap());
    let c = compile(&r, Encoding::Generic).unwrap();
    for p in prompts(&c) {
        assert!(
            p.contains(r#""context": "{\"frames\": [\"first\", \"second\"], \"position\": 7}""#),
            "{p}"
        );
    }
}

#[test]
fn tev1_compiles_each_question_independently() {
    let mut r = ticket();
    r.state = Some(Json::parse(r#"{"frames":["first","second"]}"#).unwrap());
    let c = compile(&r, Encoding::Tev1).unwrap();
    let ps = prompts(&c);
    assert_eq!(
        ps[0],
        r#"{"state": {"frames": ["first", "second"]}, "question": "Choose the department.", "options": [{"label": "A", "key": "billing", "description": "Charges and refunds"}, {"label": "B", "key": "technical", "description": "technical"}]}"#
    );
    assert_eq!(
        ps[1],
        r#"{"state": {"frames": ["first", "second"]}, "question": "{\"rule\": \"Explicit refund request?\", \"notes\": [\"Only facts\"]}", "options": [{"label": "A", "key": "false", "description": "No"}, {"label": "B", "key": "true", "description": "Yes"}]}"#
    );
    assert!(ps[2].ends_with(
        r#""options": [{"label": "A", "key": "0", "description": "Routine"}, {"label": "B", "key": "1", "description": "Urgent"}, {"label": "C", "key": "2", "description": "Emergency"}]}"#
    ));
}

#[test]
fn tev1_accepts_at_most_24_candidates() {
    for (count, ok) in [(24, true), (25, false), (26, false)] {
        let criteria: Vec<String> = (0..count).map(|i| format!(r#""{i}":"x""#)).collect();
        let r = req(&format!(
            r#"{{"model":"m","state":"s","questions":{{"q":{{"type":"choice","instructions":"Pick","criteria":{{{}}}}}}}}}"#,
            criteria.join(",")
        ));
        assert_eq!(
            compile(&r, Encoding::Tev1).is_ok(),
            ok,
            "{count} candidates"
        );
    }
}

#[test]
fn too_many_questions() {
    // Spec ALK-SP-0007.R2 scenario: 65 questions are a 400, 64 are fine.
    for (n, ok) in [(64, true), (65, false), (0, false)] {
        let qs: Vec<String> = (0..n)
            .map(|i| format!(r#""q{i}":{{"type":"noul","instructions":"Yes?"}}"#))
            .collect();
        let r = req(&format!(
            r#"{{"model":"m","state":"s","questions":{{{}}}}}"#,
            qs.join(",")
        ));
        for enc in Encoding::ALL {
            let res = compile(&r, enc);
            if ok {
                assert!(res.is_ok(), "{enc}: {res:?}");
            } else {
                assert_eq!(status(res), 400, "{enc} with {n} questions");
            }
        }
    }
}

#[test]
fn score_levels_two_to_ten_for_every_encoding() {
    for (levels, ok) in [(1, false), (2, true), (10, true), (11, false), (27, false)] {
        let mut r = ticket();
        let criteria = Json::Array(vec![Json::from("x"); levels]);
        r.questions.0[2].1.criteria = Some(criteria);
        for enc in Encoding::ALL {
            assert_eq!(compile(&r, enc).is_ok(), ok, "{enc} with {levels} levels");
        }
    }
}

#[test]
fn missing_type_defaults_to_noul() {
    let r = req(r#"{"model":"m","state":"s","questions":{"q":{"instructions":"Yes?"}}}"#);
    let c = compile(&r, Encoding::Generic).unwrap();
    assert_eq!(c.fields[0].kind.as_str(), "noul");
}

#[test]
fn invalid_requests_are_400() {
    for text in [
        r#"{}"#,
        r#"{"model":"m","state":"x","questions":{}}"#,
        r#"{"model":"m","state":" ","questions":{"x":{"type":"noul","instructions":"q"}}}"#,
        r#"{"model":"m","state":null,"questions":{"x":{"type":"noul","instructions":"q"}}}"#,
        r#"{"model":"m","questions":{"x":{"type":"noul","instructions":"q"}}}"#,
        r#"{"model":"m","state":"x","questions":{"x":{"type":"other","instructions":"q"}}}"#,
        r#"{"model":"m","state":"x","questions":{"x":{"type":"noul"}}}"#,
        r#"{"model":"m","state":"x","questions":{"":{"type":"noul","instructions":"q"}}}"#,
        r#"{"model":"m","state":"x","questions":{"x":{"type":"choice","instructions":"q","criteria":{"a":"a"}}}}"#,
        r#"{"model":"m","state":"x","questions":{"x":{"type":"choice","instructions":"q"}}}"#,
        r#"{"model":"m","state":"x","questions":{"x":{"type":"score","instructions":"q","criteria":["a",null]}}}"#,
        r#"{"model":"m","state":"x","questions":{"x":{"type":"noul","instructions":"q","criteria":{"true":null}}}}"#,
        r#"{"model":"m","state":"x","questions":{"x":{"type":"noul","instructions":"q","criteria":{"yes":"Yes"}}}}"#,
        r#"{"model":"m","state":"x","questions":{"x":{"type":"noul","instructions":"q","criteria":null}}}"#,
        r#"{"model":"m","state":42,"questions":{"x":{"type":"noul","instructions":"q"}}}"#,
    ] {
        assert_eq!(
            status(compile(&req(text), Encoding::Generic)),
            400,
            "{text}"
        );
    }
}

#[test]
fn videos_and_images_are_rejected_where_unsupported() {
    let mut r = ticket();
    r.videos = vec![Json::from("video.mp4")];
    for enc in [Encoding::Generic, Encoding::Clef] {
        let err = compile(&r, enc).unwrap_err();
        assert!(err.to_string().contains("video"), "{err}");
    }
    let mut r = ticket();
    r.images = vec!["aW1hZ2U=".into()];
    assert_eq!(status(compile(&r, Encoding::Generic)), 501);
    assert_eq!(status(compile(&r, Encoding::Kev)), 501);
    let c = compile(&r, Encoding::Clef).unwrap();
    assert_eq!(c.images, ["aW1hZ2U="]);
}

#[test]
fn head_option_texts() {
    for (kind, criteria, want) in [
        (
            "choice",
            r#"{"z":null,"a":"","b":"described"}"#,
            vec!["z", "a", "b: described"],
        ),
        (
            "score",
            r#"["low","high"]"#,
            vec!["level 0: low", "level 1: high"],
        ),
        (
            "noul",
            r#"{}"#,
            vec![
                "false: no, the statement does not hold",
                "true: yes, the statement holds",
            ],
        ),
        (
            "noul",
            r#"{"true":"present","false":"absent"}"#,
            vec!["false: absent", "true: present"],
        ),
    ] {
        let r = req(&format!(
            r#"{{"model":"m","state":"s","questions":{{"answer":{{"type":"{kind}","instructions":"Question?","criteria":{criteria}}}}}}}"#
        ));
        let c = compile(&r, Encoding::Generic).unwrap();
        assert_eq!(c.fields[0].options, want, "{kind} {criteria}");
    }
}

#[test]
fn answers_softmax_ties_and_usage() {
    let c = compile(&ticket(), Encoding::Generic).unwrap();
    let out = ScoreOutput {
        logits: vec![
            vec![0.0, 0.0],
            vec![-1000.0, 1000.0],
            vec![1000.0, 1000.0, 1000.0],
        ],
        input_tokens: 900,
        output_tokens: 0,
        cached_tokens: Some(450),
    };
    let resp = c.answer("nimble", &out).unwrap();
    let Some(Answer::Choice {
        choice, confidence, ..
    }) = resp.answers.get("department")
    else {
        panic!("choice answer expected")
    };
    assert_eq!((choice.as_str(), *confidence), ("billing", 0.0));
    assert_eq!(
        resp.answers.get("refund"),
        Some(&Answer::Noul { noul: 1.0 })
    );
    let Some(Answer::Score { score, legend, .. }) = resp.answers.get("urgency") else {
        panic!("score answer expected")
    };
    assert!((score - 1.0).abs() < 1e-12);
    assert_eq!(legend.len(), 3);
    assert_eq!(
        (resp.usage.input_tokens, resp.prompt_eval_cached_count),
        (900, Some(450))
    );
    // Answers keep question order on the wire.
    let text = serde_json::to_string(&resp).unwrap();
    let (d, r, u) = (
        text.find("department").unwrap(),
        text.find("refund").unwrap(),
        text.find("urgency").unwrap(),
    );
    assert!(d < r && r < u, "{text}");
    for bad in [
        vec![],
        vec![vec![1.0], vec![1.0, 2.0], vec![1.0, 2.0, 3.0]],
        vec![vec![f64::NAN, 0.0], vec![0.0, 1.0], vec![1.0, 2.0, 3.0]],
    ] {
        let out = ScoreOutput {
            logits: bad,
            ..Default::default()
        };
        assert!(c.answer("nimble", &out).is_err());
    }
}

#[test]
fn log_probabilities_give_the_same_answers_as_logits() {
    let c = compile(&ticket(), Encoding::Generic).unwrap();
    let a = c
        .answer(
            "m",
            &ScoreOutput {
                logits: vec![vec![0.0, 2.0], vec![0.0, -2.0], vec![0.0, 2.0, 4.0]],
                ..Default::default()
            },
        )
        .unwrap();
    let b = c
        .answer(
            "m",
            &ScoreOutput {
                logits: vec![
                    vec![-10.0, -8.0],
                    vec![-10.0, -12.0],
                    vec![-10.0, -8.0, -6.0],
                ],
                ..Default::default()
            },
        )
        .unwrap();
    let close = |x: f64, y: f64| (x - y).abs() < 1e-12;
    for ((_, x), (_, y)) in a.answers.0.iter().zip(&b.answers.0) {
        match (x, y) {
            (Answer::Noul { noul: p }, Answer::Noul { noul: q }) => assert!(close(*p, *q)),
            (
                Answer::Choice {
                    probabilities: p, ..
                },
                Answer::Choice {
                    probabilities: q, ..
                },
            )
            | (
                Answer::Score {
                    probabilities: p, ..
                },
                Answer::Score {
                    probabilities: q, ..
                },
            ) => assert!(p.iter().zip(q).all(|(p, q)| close(p.1, q.1))),
            _ => panic!("answer kinds differ"),
        }
    }
}

#[test]
fn kev_request_and_answers() {
    let r = req(
        r#"{"model":"kev:test","state":{"subject":"Charged twice","details":["café",{"count":1e6,"urgent":true}]},"questions":{"team":{"type":"choice","instructions":{"goal":"Pick a team"},"criteria":{"billing":"Payments","shipping":null}},"urgent":{"type":"noul","instructions":"Urgent?","criteria":{"true":"escalate"}},"level":{"type":"score","instructions":"How urgent?","criteria":["low",null,{"high":true}]}}}"#,
    );
    let c = compile(&r, Encoding::Kev).unwrap();
    let ScoreInput::Kev { state, questions } = &c.input else {
        panic!("kev input expected")
    };
    assert_eq!(
        state,
        "subject: Charged twice\ndetails:\n  - café\n  - count: 1000000.0\n    urgent: True"
    );
    assert_eq!(questions[0].instructions, "goal: Pick a team");
    assert_eq!(questions[0].options, ["billing: Payments", "shipping"]);
    assert_eq!(questions[1].options, ["no", "yes: escalate"]);
    assert_eq!(questions[2].options, ["low", "", "high: True"]);
    let resp = c
        .answer(
            "kev:test",
            &ScoreOutput {
                logits: vec![vec![0.0, 2.0], vec![0.0, 2.0], vec![0.0, 1.0, 0.0]],
                input_tokens: 123,
                ..Default::default()
            },
        )
        .unwrap();
    let Some(Answer::Choice {
        choice, confidence, ..
    }) = resp.answers.get("team")
    else {
        panic!()
    };
    assert_eq!(choice, "shipping");
    assert!((confidence - 0.7616).abs() < 1e-4);
    assert_eq!(
        resp.answers.get("urgent"),
        Some(&Answer::Noul { noul: 0.8808 })
    );
    let Some(Answer::Score {
        score, confidence, ..
    }) = resp.answers.get("level")
    else {
        panic!()
    };
    assert_eq!(*score, 1.0);
    assert!((confidence - 0.3642).abs() < 1e-4);
}

#[test]
fn kev_accepts_any_json_state_and_text_only() {
    for state in ["null", r#""""#, "[]"] {
        let r = req(&format!(
            r#"{{"model":"kev","state":{state},"questions":{{"q":{{"type":"choice","criteria":{{"a":null}}}}}}}}"#
        ));
        assert!(compile(&r, Encoding::Kev).is_ok(), "state {state}");
    }
    let r = req(r#"{"model":"kev","state":"x","questions":{" ":{"type":"noul"}}}"#);
    assert!(
        compile(&r, Encoding::Kev)
            .unwrap_err()
            .to_string()
            .contains("field name must not be empty")
    );
}

#[test]
fn clef_joint_prompt_segments() {
    let r = req(
        r#"{"model":"renamed-model","state":{"z":2,"a":"café & <x>"},"questions":{"team":{"type":"choice","instructions":"Route it","criteria":{"z":null,"a":"First"}},"yes":{"type":"noul","instructions":{"z":2,"a":1}},"rating":{"type":"score","instructions":"Rate it","criteria":["Low","High"]}}}"#,
    );
    let c = compile(&r, Encoding::Clef).unwrap();
    let ScoreInput::Clef {
        segments,
        fields,
        image_position,
    } = &c.input
    else {
        panic!("clef input expected")
    };
    assert_eq!(*image_position, 1);
    assert_eq!(segments[1], r#"{"a":"café & <x>","z":2}"#);
    let option =
        |f: usize, i: usize| segments[fields[f].options[i][0]..fields[f].options[i][1]].concat();
    assert_eq!(option(0, 0), r#"{"description":"First","option_id":"a"}"#);
    assert_eq!(option(0, 1), r#"{"option_id":"z"}"#);
    assert_eq!(
        option(1, 0),
        r#"{"description":"The proposition is true or the answer is yes.","option_id":"true"}"#
    );
    assert_eq!(fields.iter().map(|f| f.kind).collect::<Vec<_>>(), [1, 0, 2]);
    let prompt = segments.concat();
    assert!(prompt.ends_with("<think>\n\n</think>\n\nJOINT SCHEMA DECISIONS:"));
    assert!(prompt.contains("INSTRUCTION: {\"a\":1,\"z\":2}\nALLOWED OPTIONS:\n"));
    assert!(prompt.contains("\nFIELD 1\nID: team\nTYPE: choice\nINSTRUCTION: Route it"));
    // Choices are back in request order for the answer; the head's sorted order is undone.
    assert_eq!(c.fields[0].choices[0].value, ChoiceValue::Key("z".into()));
    let resp = c
        .answer(
            "renamed-model",
            &ScoreOutput {
                logits: vec![vec![2.0, 0.0], vec![2.0, 0.0], vec![0.0, 2.0]],
                input_tokens: 99,
                ..Default::default()
            },
        )
        .unwrap();
    let Some(Answer::Choice {
        choice, confidence, ..
    }) = resp.answers.get("team")
    else {
        panic!()
    };
    assert_eq!(choice, "a");
    assert!((confidence - 0.472_934_658_996_838_4).abs() < 1e-8);
    let Some(Answer::Noul { noul }) = resp.answers.get("yes") else {
        panic!()
    };
    assert!(*noul > 0.88);
}

#[test]
fn clef_null_state_fallback_instructions_and_json_descriptions() {
    let r = req(
        r#"{"model":"clef","state":null,"questions":{"team":{"type":"choice","criteria":{"z":null,"a":{"text":"First","weight":1e6}}},"yes":{"type":"noul","instructions":null,"criteria":{"true":null}},"rating":{"type":"score","instructions":"","criteria":[null,{"text":"High"}]}}}"#,
    );
    let c = compile(&r, Encoding::Clef).unwrap();
    let ScoreInput::Clef {
        segments, fields, ..
    } = &c.input
    else {
        panic!()
    };
    assert_eq!(segments[1], "null");
    for (i, name) in ["team", "yes", "rating"].iter().enumerate() {
        assert_eq!(&segments[fields[i].question[0]], name);
    }
    for (f, o, want) in [
        (
            0,
            0,
            r#"{"description":{"text":"First","weight":1000000.0},"option_id":"a"}"#,
        ),
        (0, 1, r#"{"option_id":"z"}"#),
        (1, 0, r#"{"option_id":"true"}"#),
        (2, 0, r#"{"option_id":"0"}"#),
        (2, 1, r#"{"description":{"text":"High"},"option_id":"1"}"#),
    ] {
        assert_eq!(segments[fields[f].options[o][0]], want);
    }
    let resp = c
        .answer(
            "clef",
            &ScoreOutput {
                logits: vec![vec![2.0, 0.0], vec![2.0, 0.0], vec![0.0, 2.0]],
                ..Default::default()
            },
        )
        .unwrap();
    let Some(Answer::Score { legend, .. }) = resp.answers.get("rating") else {
        panic!()
    };
    let legend = Json::Object(legend.clone()).to_json_string();
    assert_eq!(legend, r#"{"0":null,"1":{"text":"High"}}"#);
    // The letter encodings do not accept non-string descriptions.
    assert!(compile(&r, Encoding::Generic).is_err());
}

#[test]
fn clef_rejects_invalid_criteria() {
    for q in [
        r#"{"type":"choice"}"#,
        r#"{"type":"choice","criteria":{"":"empty","valid":"valid"}}"#,
        r#"{"type":"choice","criteria":{"a":null}}"#,
        r#"{"type":"noul","criteria":{"yes":"invalid"}}"#,
        r#"{"type":"noul","criteria":[]}"#,
        r#"{"type":"score","criteria":{"a":"invalid"}}"#,
        r#"{"type":"score","criteria":["one"]}"#,
        r#"{"type":"unknown"}"#,
    ] {
        let r = req(&format!(
            r#"{{"model":"clef","state":"x","questions":{{"q":{q}}}}}"#
        ));
        assert_eq!(status(compile(&r, Encoding::Clef)), 400, "{q}");
    }
}

#[test]
fn strands_pointer_rows() {
    let r = req(
        r#"{"model":"renamed","state":{"z":1e6,"a":["café & <x>",true,null]},"questions":{"team":{"type":"choice","instructions":"  Route it  ","criteria":{"z":" Last\n team ","a":null}},"yes":{"type":"noul","instructions":"Is it urgent?"},"rating":{"type":"score","instructions":["Rate it"],"criteria":["Low","High"]}}}"#,
    );
    let c = compile(&r, Encoding::Strands).unwrap();
    let ScoreInput::Strands(rows) = &c.input else {
        panic!("strands input expected")
    };
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0].prefix,
        "<state>\n{\n  \"z\": 1000000.0,\n  \"a\": [\n    \"café & <x>\",\n    true,\n    null\n  ]\n}\n</state>\n"
    );
    assert_eq!(
        rows[0].prompt,
        "<question type=\"choice\">\nSelect exactly one option.\nRoute it\n<options>\n1. z — Last team\n2. a\n</options>\n</question>\n<answer>"
    );
    for (i, want) in ["1. z — Last team", "2. a"].iter().enumerate() {
        let [s, e] = rows[0].options[i];
        assert_eq!(&rows[0].prompt[s..e], *want);
    }
    assert_eq!(rows.iter().map(|r| r.kind).collect::<Vec<_>>(), [1, 0, 2]);
    assert!(rows[1].prompt.contains(
        "1. false — the statement does not hold for this state\n2. true — the statement holds for this state"
    ));
    assert!(rows[2].prompt.contains("[\n  \"Rate it\"\n]"));
    let resp = c
        .answer(
            "renamed",
            &ScoreOutput {
                logits: vec![vec![0.0, 2.0], vec![0.0, 2.0], vec![0.0, 2.0]],
                input_tokens: 99,
                ..Default::default()
            },
        )
        .unwrap();
    let Some(Answer::Choice {
        choice, confidence, ..
    }) = resp.answers.get("team")
    else {
        panic!()
    };
    assert_eq!(choice, "a");
    let Some(Answer::Score {
        score,
        confidence: sc,
        ..
    }) = resp.answers.get("rating")
    else {
        panic!()
    };
    assert!((score - 0.880_797_077_977_882_3).abs() < 1e-8);
    assert!((sc - confidence).abs() < 1e-8);
}

#[test]
fn model_is_required() {
    let mut r = ticket();
    r.model = "  ".into();
    for enc in Encoding::ALL {
        assert_eq!(status(compile(&r, enc)), 400);
    }
}
