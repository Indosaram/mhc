use crate::models::{self, ModelsClassifierOptions};
use crate::models_generated::{CLASSIFIER_MODELS, get_builtin_classifier_model};
use crate::types::{
    AnyModel, ClassifierAnswer, ClassifierContext, ClassifierOptions, ClassifierStopReason,
    ModelType, ProviderRequestOptions,
};
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn context() -> ClassifierContext {
    serde_json::from_value(json!({"state":{"value":7},"questions":{
        "ready":{"type":"bool","instructions":"ready?","criteria":{"true":"ready","false":"not ready"}},
        "rank":{"type":"score","instructions":"rank?","criteria":["low","high"]},
        "route":{"type":"choice","instructions":"route?","criteria":{"a":"first","b":"second"}}
    }})).expect("classifier context")
}

fn options() -> ClassifierOptions {
    ClassifierOptions {
        request: ProviderRequestOptions {
            api_key: Some("test-key".into()),
            max_retries: Some(0),
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn server(
    app: Router,
) -> (
    String,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind server");
    let address = listener.local_addr().expect("server address");
    let (shutdown, signal) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                signal.await.expect("shutdown signal");
            })
            .await
            .expect("serve classifier");
    });
    (format!("http://{address}"), shutdown, handle)
}

async fn shutdown(shutdown: tokio::sync::oneshot::Sender<()>, task: tokio::task::JoinHandle<()>) {
    shutdown.send(()).expect("send shutdown");
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("bounded server shutdown")
        .expect("server task");
}

#[tokio::test]
async fn system_one_maps_all_answer_types_and_prices_reported_usage() {
    let observed = Arc::new(Mutex::new(None));
    let captured = observed.clone();
    let app = Router::new().route("/v1/systemone", post(move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
        *captured.lock().expect("capture") = Some((headers, body));
        async { Json(json!({"answers":{
            "ready":{"type":"noul","noul":0.8},"rank":{"type":"score","score":0.4,"confidence":0.2},
            "route":{"type":"choice","choice":"b","probabilities":{"a":0.1,"b":0.9},"confidence":0.8}
        },"usage":{"input_tokens":1000000,"output_tokens":10}})) }
    }));
    let (root, stop, task) = server(app).await;
    let mut model = get_builtin_classifier_model("vercel-ai-gateway", "liquid/d1")
        .expect("catalog model")
        .clone();
    model.base_url = format!("{root}/v1/");

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        models::typesafe_system_one::classify(&model, &context(), Some(options())),
    )
    .await
    .expect("bounded classification");

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers["ready"],
        ClassifierAnswer::Bool { probability: 0.8 }
    );
    assert!(matches!(
        result.answers["rank"],
        ClassifierAnswer::Score {
            score: 0.4,
            confidence: 0.2
        }
    ));
    assert!(
        matches!(&result.answers["route"], ClassifierAnswer::Choice { choice, .. } if choice == "b")
    );
    assert_eq!(result.usage.expect("billed usage").cost.input, 0.02);
    let (headers, body) = observed.lock().expect("capture").take().expect("request");
    assert_eq!(headers["authorization"], "Bearer test-key");
    assert_eq!(body["questions"]["ready"]["type"], "noul");
    assert_eq!(body["model"], model.id);
    shutdown(stop, task).await;
}

#[tokio::test]
async fn billed_usage_survives_invalid_answers() {
    let app = Router::new().route(
        "/systemone",
        post(|| async { Json(json!({"answers":{},"usage":{"input_tokens":12}})) }),
    );
    let (root, stop, task) = server(app).await;
    let mut model = get_builtin_classifier_model("typesafe", "jev-latest")
        .expect("model")
        .clone();
    model.base_url = root;

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        models::typesafe_system_one::classify(&model, &context(), Some(options())),
    )
    .await
    .expect("bounded classification");

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(result.answers.is_empty());
    assert_eq!(result.usage.expect("billed usage").input, 12);
    shutdown(stop, task).await;
}

#[tokio::test]
async fn cloudflare_rejects_incomplete_run_envelopes() {
    let app = Router::new().route(
        "/run",
        post(|| async {
            Json(json!({"success":true,"result":{"state":"Running","result":{"answers":{}}}}))
        }),
    );
    let (root, stop, task) = server(app).await;
    let mut model = get_builtin_classifier_model("cloudflare-workers-ai", "typesafe/jev")
        .expect("model")
        .clone();
    model.base_url = root;

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        models::cloudflare_workers_ai_system_one::classify(&model, &context(), Some(options())),
    )
    .await
    .expect("bounded classification");

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(result.answers.is_empty());
    shutdown(stop, task).await;
}

#[tokio::test]
async fn collection_classification_uses_auth_transforms_and_classifier_hooks() {
    let observed = Arc::new(Mutex::new(None));
    let captured = observed.clone();
    let app = Router::new().route(
        "/systemone",
        post(
            move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                *captured.lock().expect("capture") = Some((headers, body));
                async { Json(json!({"answers":{}})) }
            },
        ),
    );
    let (root, stop, task) = server(app).await;
    let collection = models::create_models(None);
    collection.set_provider(models::typesafe_provider().expect("typesafe provider"));
    let mut model = collection.get_classifier_models(Some("typesafe")).remove(0);
    model.base_url = root;
    let response_seen = Arc::new(Mutex::new(None));
    let captured_response = response_seen.clone();
    let mut options = options();
    options.on_payload = Some(Arc::new(|mut body, model| {
        Box::pin(async move {
            body["catalogModel"] = json!(model.id);
            Ok(Some(body))
        })
    }));
    options.on_response = Some(Arc::new(move |response, _| {
        *captured_response.lock().expect("response capture") = Some(response.status);
        Box::pin(async { Ok(()) })
    }));
    let transforms = models::ModelsRequestTransforms {
        transform_headers: Some(Arc::new(|mut headers| {
            Box::pin(async move {
                headers.insert("x-transformed".into(), Some("yes".into()));
                headers
            })
        })),
    };

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        collection.classify(
            &model,
            &ClassifierContext::default(),
            Some(ModelsClassifierOptions {
                options,
                transforms,
            }),
        ),
    )
    .await
    .expect("bounded classification");

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    let (headers, body) = observed.lock().expect("capture").take().expect("request");
    assert_eq!(headers["x-transformed"], "yes");
    assert_eq!(body["catalogModel"], model.id);
    assert_eq!(*response_seen.lock().expect("response"), Some(200));
    shutdown(stop, task).await;
}

#[test]
fn mixed_catalog_parses_classifier_without_chat_fields() {
    let model = get_builtin_classifier_model("typesafe", "jev-latest").expect("model");

    let parsed: AnyModel = serde_json::from_value(serde_json::to_value(model).expect("serialize"))
        .expect("mixed catalog entry");

    assert_eq!(parsed.model_type(), ModelType::Classifier);
    for model in CLASSIFIER_MODELS.values().flat_map(|models| models.values()) {
        let value = serde_json::to_value(model).expect("serialize classifier metadata");
        assert!(value.get("reasoning").is_none(), "{}", model.id);
        assert!(value.get("maxTokens").is_none(), "{}", model.id);
        let parsed: AnyModel = serde_json::from_value(value).expect("mixed classifier entry");
        assert_eq!(parsed, AnyModel::Classifier(model.clone()));
    }
    assert_eq!(
        CLASSIFIER_MODELS
            .values()
            .map(|models| models.len())
            .sum::<usize>(),
        48
    );
    assert!(serde_json::from_value::<AnyModel>(json!({"type":"unknown"})).is_err());
}

#[tokio::test]
async fn llama_escalates_missing_labels_and_caches_tokenization() {
    let depths = Arc::new(Mutex::new(Vec::new()));
    let observed_depths = depths.clone();
    let tokenizations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_tokens = tokenizations.clone();
    let app = Router::new()
        .route(
            "/tokenize",
            post(move |Json(body): Json<Value>| {
                observed_tokens.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let tokens = match body["content"].as_str().expect("token content") {
                    "\n" => vec![10],
                    "\nYes" => vec![10, 20],
                    "\nNo" => vec![10, 30],
                    _ => panic!("unexpected tokenization request"),
                };
                async move { Json(json!({"tokens":tokens})) }
            }),
        )
        .route(
            "/apply-template",
            post(|Json(body): Json<Value>| async move {
                assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
                Json(json!({"prompt":"template<think>"}))
            }),
        )
        .route(
            "/completion",
            post(move |Json(body): Json<Value>| {
                let depth = body["n_probs"].as_u64().expect("readout depth");
                observed_depths.lock().expect("depth capture").push(depth);
                assert_eq!(body["prompt"], "template<think></think>");
                assert_eq!(body["n_predict"], 1);
                assert_eq!(body["post_sampling_probs"], false);
                let entries = if depth == 256 {
                    json!([{"id":20,"logprob":-2.0}])
                } else {
                    json!([{"id":20,"logprob":-2.0},{"id":30,"logprob":-2.0}])
                };
                async move { Json(json!({"completion_probabilities":[{"top_logprobs":entries}]})) }
            }),
        );
    let (root, stop, task) = server(app).await;
    let mut model = get_builtin_classifier_model("typesafe", "jev-latest")
        .expect("model")
        .clone();
    model.api = "llama-cpp-classify".into();
    model.base_url = format!("{root}/v1/");
    let context: ClassifierContext = serde_json::from_value(json!({"state":{},"questions":{
        "first":{"type":"bool","instructions":"first?","criteria":{"true":"yes","false":"no"}},
        "second":{"type":"bool","instructions":"second?","criteria":{"true":"yes","false":"no"}}
    }}))
    .expect("context");

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        models::llama_cpp_classify::classify(&model, &context, Some(options())),
    )
    .await
    .expect("bounded llama readout");

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers["first"],
        ClassifierAnswer::Bool { probability: 0.5 }
    );
    assert_eq!(
        result.answers["second"],
        ClassifierAnswer::Bool { probability: 0.5 }
    );
    assert_eq!(*depths.lock().expect("depths"), vec![256, 4096, 256, 4096]);
    assert_eq!(tokenizations.load(std::sync::atomic::Ordering::SeqCst), 4);
    shutdown(stop, task).await;
}

#[tokio::test]
async fn classifier_cancellation_interrupts_a_pending_response() {
    let (entered, entered_signal) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let release = Arc::new(tokio::sync::Notify::new());
    let handler_release = release.clone();
    let app = Router::new().route(
        "/systemone",
        post(move || {
            entered
                .lock()
                .expect("entry signal")
                .take()
                .expect("single request")
                .send(())
                .expect("entry receiver");
            let release = handler_release.clone();
            async move {
                release.notified().await;
                Json(json!({"answers":{}}))
            }
        }),
    );
    let (root, stop, task) = server(app).await;
    let mut model = get_builtin_classifier_model("typesafe", "jev-latest")
        .expect("model")
        .clone();
    model.base_url = root;
    let controller = crate::utils::abort::AbortController::new();
    let mut options = options();
    options.request.signal = Some(controller.signal());

    let classify = tokio::spawn(async move {
        models::typesafe_system_one::classify(&model, &ClassifierContext::default(), Some(options))
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), entered_signal)
        .await
        .expect("bounded request entry")
        .expect("request entered");
    controller.abort(None);
    let result = tokio::time::timeout(Duration::from_secs(5), classify)
        .await
        .expect("bounded cancellation")
        .expect("classification task");

    assert_eq!(result.stop_reason, ClassifierStopReason::Aborted);
    assert!(result.answers.is_empty());
    release.notify_one();
    shutdown(stop, task).await;
}

#[test]
fn llama_readout_uses_temperature_and_first_tie_winner() {
    let question = &context().questions["route"];
    let keys = vec!["a".to_owned(), "b".to_owned()];

    let answer = models::llama_cpp_classify::answer_from_probabilities(
        question,
        &keys,
        &models::llama_cpp_classify::label_probabilities(&[-2.0, -2.0], 0.5),
    );

    assert_eq!(
        answer,
        ClassifierAnswer::Choice {
            choice: "a".into(),
            probabilities: indexmap::IndexMap::from([("a".into(), 0.5), ("b".into(), 0.5)]),
            confidence: 0.0
        }
    );
}

#[tokio::test]
async fn llama_rejects_invalid_temperature_before_requesting() {
    let mut model = get_builtin_classifier_model("typesafe", "jev-latest")
        .expect("model")
        .clone();
    model.api = "llama-cpp-classify".into();
    model.base_url = "not-a-url".into();

    let result = models::llama_cpp_classify::classify(
        &model,
        &context(),
        Some(ClassifierOptions {
            temperature: Some(0.0),
            ..Default::default()
        }),
    )
    .await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(result.answers.is_empty());
}
