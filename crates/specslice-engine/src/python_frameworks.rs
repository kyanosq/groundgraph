//! P17 — Python framework decorator classifier.
//!
//! Python projects rely heavily on decorators to register framework
//! entry points (HTTP routes, background tasks, CLI commands).
//! SpecSlice's AST scanner already captures those decorators
//! verbatim ([`crate::python_ast::PythonSymbol::decorators`]); this
//! module turns that raw text into a structured
//! [`FrameworkRole`] so the rest of the engine can:
//!
//! - flag the wrapped symbol as a framework entry point in dead-code
//!   reachability,
//! - attach human-readable metadata (method, path, queue, group, …)
//!   to the symbol node for search / inspector surfaces,
//! - keep the classification source-of-truth in one file so adding
//!   a new framework only touches `match_decorator`.
//!
//! Confidence levels intentionally remain conservative: the
//! classifier only fires on shapes that are unambiguous (e.g.
//! `@router.get(...)` is always a FastAPI/Starlette/APIRouter
//! route, `@app.task` is always a Celery task). Anything more
//! exotic stays untagged so callers cannot accidentally promote
//! a "maybe" framework hit into a strong fact.
//!
//! The classifier is pure — no I/O, no panics on malformed input —
//! so callers can run it inside hot AST loops without worrying
//! about side effects.

use serde::{Deserialize, Serialize};

/// Structured framework role assigned to a Python symbol based on
/// the decorators above it. Variants stay flat so the JSON form
/// stored in [`specslice_core::Node::metadata_json`] is easy to
/// consume from MCP / CLI clients without round-tripping through
/// Rust types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "framework", rename_all = "snake_case")]
pub enum FrameworkRole {
    /// FastAPI / Starlette / APIRouter HTTP handler.
    /// `verb` is one of `get / post / put / delete / patch / head /
    /// options / websocket / api_route`. `path` is the literal route
    /// passed as the first positional argument (best-effort), e.g.
    /// `/items` or `""` when we cannot extract it.
    FastapiRoute {
        verb: String,
        path: Option<String>,
        decorator: String,
    },
    /// Flask `@app.route("/...")` or `@blueprint.route("/...")`.
    /// `methods` is `None` unless the decorator explicitly listed
    /// HTTP verbs.
    FlaskRoute {
        path: Option<String>,
        methods: Option<Vec<String>>,
        decorator: String,
    },
    /// Django `@require_http_methods([...])` /
    /// `@login_required` / class-based view. Coarse-grained: we
    /// only care that the function is a framework entry point.
    DjangoView { decorator: String },
    /// Celery (`@app.task`, `@shared_task`) or RQ (`@job`) task.
    BackgroundTask {
        runtime: BackgroundTaskRuntime,
        queue: Option<String>,
        decorator: String,
    },
    /// Click / Typer command (`@click.command`, `@app.command`).
    CliCommand {
        runtime: CliRuntime,
        decorator: String,
    },
    /// FastAPI startup / shutdown / lifespan event hook.
    EventHandler { event: String, decorator: String },
    /// FastAPI / Starlette `@app.exception_handler(ExcCls)` /
    /// `@app.middleware("http")` — framework-invoked entry points
    /// that bind by type / kind rather than by URL path. Catching
    /// these is what trimmed ~40 false-positive dead-code hits on
    /// the atagent FastAPI backend during P17 validation.
    AsgiInfrastructure { kind: String, decorator: String },
    /// SQLAlchemy ORM event (`@event.listens_for`).
    SqlAlchemyEvent { decorator: String },
    /// Pydantic field / model validator.
    PydanticValidator { decorator: String },
    /// `dataclasses.dataclass` (and `attr.s`, `attrs.define`).
    DataClass { runtime: DataClassRuntime },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskRuntime {
    Celery,
    Rq,
    Dramatiq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliRuntime {
    Click,
    Typer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataClassRuntime {
    Stdlib,
    Attrs,
}

impl FrameworkRole {
    /// Stable identifier for the role family — used by dead-code /
    /// search filters that want a single word ("fastapi_route",
    /// "background_task", …) instead of the structured payload.
    pub fn family(&self) -> &'static str {
        match self {
            FrameworkRole::FastapiRoute { .. } => "fastapi_route",
            FrameworkRole::FlaskRoute { .. } => "flask_route",
            FrameworkRole::DjangoView { .. } => "django_view",
            FrameworkRole::BackgroundTask { .. } => "background_task",
            FrameworkRole::CliCommand { .. } => "cli_command",
            FrameworkRole::EventHandler { .. } => "event_handler",
            FrameworkRole::AsgiInfrastructure { .. } => "asgi_infrastructure",
            FrameworkRole::SqlAlchemyEvent { .. } => "sqlalchemy_event",
            FrameworkRole::PydanticValidator { .. } => "pydantic_validator",
            FrameworkRole::DataClass { .. } => "data_class",
        }
    }

    /// True when this role represents an externally-triggered entry
    /// point — i.e. dead-code reachability should treat the wrapped
    /// symbol as reachable even if no in-repo caller exists.
    pub fn is_framework_entrypoint(&self) -> bool {
        matches!(
            self,
            FrameworkRole::FastapiRoute { .. }
                | FrameworkRole::FlaskRoute { .. }
                | FrameworkRole::DjangoView { .. }
                | FrameworkRole::BackgroundTask { .. }
                | FrameworkRole::CliCommand { .. }
                | FrameworkRole::EventHandler { .. }
                | FrameworkRole::AsgiInfrastructure { .. }
                | FrameworkRole::SqlAlchemyEvent { .. }
                | FrameworkRole::PydanticValidator { .. }
        )
    }
}

/// Pick the strongest role implied by a decorator stack. Decorators
/// are tried in order; the first match wins, mirroring how Python
/// applies decorators bottom-up but still keeps the outermost
/// framework as the canonical role.
pub fn classify_decorators(decorators: &[String]) -> Option<FrameworkRole> {
    decorators.iter().find_map(|d| classify_decorator(d))
}

/// Classify a single decorator string (without the leading `@`).
pub fn classify_decorator(raw: &str) -> Option<FrameworkRole> {
    let decorator = raw.trim();
    if decorator.is_empty() {
        return None;
    }
    let (head, args) = split_decorator(decorator);
    let head_norm = head;

    // --- FastAPI / Starlette / APIRouter --------------------------------
    if let Some(verb) = match_fastapi_verb(head_norm) {
        let path = first_string_arg(args);
        return Some(FrameworkRole::FastapiRoute {
            verb: verb.to_string(),
            path,
            decorator: decorator.to_string(),
        });
    }

    // --- Flask blueprint / app routes -----------------------------------
    if matches_object_method(head_norm, &["app", "bp", "blueprint", "router"], "route") {
        let path = first_string_arg(args);
        let methods = extract_methods_kwarg(args);
        return Some(FrameworkRole::FlaskRoute {
            path,
            methods,
            decorator: decorator.to_string(),
        });
    }

    // --- Django view decorators ----------------------------------------
    if matches!(
        head_norm,
        "login_required"
            | "permission_required"
            | "user_passes_test"
            | "require_http_methods"
            | "require_GET"
            | "require_POST"
            | "csrf_exempt"
            | "api_view"
    ) {
        return Some(FrameworkRole::DjangoView {
            decorator: decorator.to_string(),
        });
    }

    // --- Celery / RQ / Dramatiq tasks ----------------------------------
    if matches_object_method(head_norm, &["app", "celery"], "task")
        || head_norm == "shared_task"
        || head_norm == "celery_app.task"
    {
        let queue = extract_kwarg(args, "queue").map(strip_quotes);
        return Some(FrameworkRole::BackgroundTask {
            runtime: BackgroundTaskRuntime::Celery,
            queue,
            decorator: decorator.to_string(),
        });
    }
    if head_norm == "job" || head_norm == "rq.job" {
        return Some(FrameworkRole::BackgroundTask {
            runtime: BackgroundTaskRuntime::Rq,
            queue: extract_kwarg(args, "queue").map(strip_quotes),
            decorator: decorator.to_string(),
        });
    }
    if matches_object_method(head_norm, &["dramatiq", "actor"], "actor") || head_norm == "actor" {
        return Some(FrameworkRole::BackgroundTask {
            runtime: BackgroundTaskRuntime::Dramatiq,
            queue: extract_kwarg(args, "queue").map(strip_quotes),
            decorator: decorator.to_string(),
        });
    }

    // --- Click / Typer CLI commands ------------------------------------
    if head_norm == "click.command" || head_norm == "click.group" {
        return Some(FrameworkRole::CliCommand {
            runtime: CliRuntime::Click,
            decorator: decorator.to_string(),
        });
    }
    if matches_object_method(head_norm, &["app", "cli", "typer_app"], "command")
        || matches_object_method(head_norm, &["app", "cli", "typer_app"], "callback")
    {
        // Typer's `app.command()` is the canonical shape but it
        // shares syntax with FastAPI's `app.command` is unusual.
        // We disambiguate by looking at the bare `head`: if the
        // module path is FastAPI-rooted ("router.command" would be
        // weird) we still call it a CLI command — production code
        // tends to name the Typer app `app` or `cli`.
        return Some(FrameworkRole::CliCommand {
            runtime: CliRuntime::Typer,
            decorator: decorator.to_string(),
        });
    }

    // --- FastAPI lifecycle events --------------------------------------
    if head_norm == "app.on_event" || head_norm == "router.on_event" {
        let event = first_string_arg(args).unwrap_or_else(|| "unknown".to_string());
        return Some(FrameworkRole::EventHandler {
            event,
            decorator: decorator.to_string(),
        });
    }

    // --- FastAPI exception handlers / middleware -----------------------
    // Both bind by type/kind instead of by URL path, but they are
    // still framework-invoked entry points the user never calls
    // directly. atagent has ~5 exception_handlers and ~3 middleware
    // declarations that would otherwise look dead.
    if matches_object_method(head_norm, &["app", "router"], "exception_handler") {
        return Some(FrameworkRole::AsgiInfrastructure {
            kind: "exception_handler".into(),
            decorator: decorator.to_string(),
        });
    }
    if matches_object_method(head_norm, &["app", "router"], "middleware") {
        return Some(FrameworkRole::AsgiInfrastructure {
            kind: "middleware".into(),
            decorator: decorator.to_string(),
        });
    }

    // --- SQLAlchemy event listener -------------------------------------
    if head_norm == "event.listens_for" || head_norm == "listens_for" {
        return Some(FrameworkRole::SqlAlchemyEvent {
            decorator: decorator.to_string(),
        });
    }

    // --- Pydantic validators -------------------------------------------
    if matches!(
        head_norm,
        "validator"
            | "field_validator"
            | "model_validator"
            | "root_validator"
            | "pydantic.validator"
            | "pydantic.field_validator"
            | "pydantic.model_validator"
    ) {
        return Some(FrameworkRole::PydanticValidator {
            decorator: decorator.to_string(),
        });
    }

    // --- Dataclasses ---------------------------------------------------
    if matches!(head_norm, "dataclass" | "dataclasses.dataclass") {
        return Some(FrameworkRole::DataClass {
            runtime: DataClassRuntime::Stdlib,
        });
    }
    if matches!(
        head_norm,
        "attr.s" | "attrs.define" | "attrs.frozen" | "attr.define"
    ) {
        return Some(FrameworkRole::DataClass {
            runtime: DataClassRuntime::Attrs,
        });
    }

    None
}

fn match_fastapi_verb(head: &str) -> Option<&str> {
    let (object, method) = head.rsplit_once('.')?;
    // FastAPI routers can be named anything (`app`, `router`,
    // `api_router`, …) so we check the verb whitelist instead of
    // the object. `router.get("/")` is still a route, even if
    // `router` is a custom name.
    let verb = match method {
        "get" => "get",
        "post" => "post",
        "put" => "put",
        "delete" => "delete",
        "patch" => "patch",
        "head" => "head",
        "options" => "options",
        "websocket" => "websocket",
        "api_route" => "api_route",
        _ => return None,
    };
    // Guard against plain `time.get(...)` or `httpx.get(...)`
    // attribute calls that happen to look like a verb: we require
    // the object name to look like a router. Common router names
    // include `app`, `router`, `api`, anything ending in `_router`
    // or `_app`, or namespaced versions like `routes.user_router`.
    let last = object.rsplit('.').next().unwrap_or(object);
    if matches!(last, "app" | "router" | "api" | "blueprint" | "bp")
        || last.ends_with("_router")
        || last.ends_with("_app")
        || last.ends_with("Router")
    {
        Some(verb)
    } else {
        None
    }
}

fn matches_object_method(head: &str, objects: &[&str], method: &str) -> bool {
    let Some((object, m)) = head.rsplit_once('.') else {
        return false;
    };
    if m != method {
        return false;
    }
    let last = object.rsplit('.').next().unwrap_or(object);
    objects.contains(&last)
}

fn split_decorator(raw: &str) -> (&str, &str) {
    match raw.find('(') {
        Some(idx) => (&raw[..idx], &raw[idx..]),
        None => (raw, ""),
    }
}

fn first_string_arg(args: &str) -> Option<String> {
    let inner = args.trim().strip_prefix('(')?;
    let inner = inner.trim_end_matches(')');
    let candidate = inner.split(',').next()?.trim();
    if candidate.is_empty() {
        return None;
    }
    let value = strip_quotes(candidate.to_string());
    if value.is_empty() || value == candidate {
        // Not a literal string — return as-is so callers see whatever
        // the source had (e.g. `path` for `app.get(path)`).
        Some(candidate.to_string())
    } else {
        Some(value)
    }
}

fn extract_kwarg(args: &str, key: &str) -> Option<String> {
    let inner = args.trim().strip_prefix('(')?.trim_end_matches(')');
    for part in inner.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

fn extract_methods_kwarg(args: &str) -> Option<Vec<String>> {
    let raw = extract_kwarg(args, "methods")?;
    let inner = raw
        .trim()
        .strip_prefix('[')
        .or_else(|| raw.trim().strip_prefix('('))?
        .trim_end_matches(']')
        .trim_end_matches(')');
    let methods: Vec<String> = inner
        .split(',')
        .map(|s| strip_quotes(s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect();
    if methods.is_empty() {
        None
    } else {
        Some(methods)
    }
}

fn strip_quotes(value: String) -> String {
    let trimmed = value.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_fastapi_router_verbs() {
        let role = classify_decorator("router.get(\"/items\", tags=[\"catalog\"])").unwrap();
        match role {
            FrameworkRole::FastapiRoute { verb, path, .. } => {
                assert_eq!(verb, "get");
                assert_eq!(path.as_deref(), Some("/items"));
            }
            other => panic!("expected FastapiRoute, got {other:?}"),
        }
        // App-level prefix also works, including websocket and the
        // generic `api_route`.
        assert_eq!(
            classify_decorator("app.websocket(\"/ws\")")
                .unwrap()
                .family(),
            "fastapi_route"
        );
        assert_eq!(
            classify_decorator("app.api_route(\"/health\", methods=[\"GET\"])")
                .unwrap()
                .family(),
            "fastapi_route"
        );
        // Unrelated `.get` calls (httpx, requests, dict) should NOT
        // match because the object is not a router-shape name.
        assert!(classify_decorator("httpx.get(\"/items\")").is_none());
        assert!(classify_decorator("os.get(\"PATH\")").is_none());
    }

    #[test]
    fn classifies_flask_routes_with_methods() {
        let role = classify_decorator("app.route(\"/login\", methods=[\"POST\"])").unwrap();
        match role {
            FrameworkRole::FlaskRoute { path, methods, .. } => {
                assert_eq!(path.as_deref(), Some("/login"));
                assert_eq!(methods, Some(vec!["POST".to_string()]));
            }
            other => panic!("expected FlaskRoute, got {other:?}"),
        }
        assert_eq!(
            classify_decorator("bp.route(\"/x\")").unwrap().family(),
            "flask_route"
        );
    }

    #[test]
    fn classifies_celery_and_rq_jobs() {
        let role = classify_decorator("shared_task").unwrap();
        match role {
            FrameworkRole::BackgroundTask { runtime, .. } => {
                assert_eq!(runtime, BackgroundTaskRuntime::Celery);
            }
            other => panic!("expected Celery BackgroundTask, got {other:?}"),
        }
        let role = classify_decorator("app.task(queue=\"emails\")").unwrap();
        match role {
            FrameworkRole::BackgroundTask { runtime, queue, .. } => {
                assert_eq!(runtime, BackgroundTaskRuntime::Celery);
                assert_eq!(queue.as_deref(), Some("emails"));
            }
            other => panic!("expected Celery BackgroundTask, got {other:?}"),
        }
        let role = classify_decorator("job(queue=\"high\")").unwrap();
        assert!(matches!(
            role,
            FrameworkRole::BackgroundTask {
                runtime: BackgroundTaskRuntime::Rq,
                ..
            }
        ));
    }

    #[test]
    fn classifies_click_and_typer_commands() {
        let click = classify_decorator("click.command").unwrap();
        assert!(matches!(
            click,
            FrameworkRole::CliCommand {
                runtime: CliRuntime::Click,
                ..
            }
        ));
        let typer = classify_decorator("app.command(\"run\")").unwrap();
        assert!(matches!(
            typer,
            FrameworkRole::CliCommand {
                runtime: CliRuntime::Typer,
                ..
            }
        ));
    }

    #[test]
    fn classifies_asgi_infrastructure() {
        let handler = classify_decorator("app.exception_handler(BaseCustomException)").unwrap();
        match handler {
            FrameworkRole::AsgiInfrastructure { kind, .. } => {
                assert_eq!(kind, "exception_handler");
            }
            other => panic!("expected AsgiInfrastructure handler, got {other:?}"),
        }
        let middleware = classify_decorator("app.middleware(\"http\")").unwrap();
        match middleware {
            FrameworkRole::AsgiInfrastructure { kind, .. } => {
                assert_eq!(kind, "middleware");
            }
            other => panic!("expected AsgiInfrastructure middleware, got {other:?}"),
        }
        // Both must register as framework entry points so dead-code
        // does not flag them.
        assert!(classify_decorator("app.exception_handler(Exception)")
            .unwrap()
            .is_framework_entrypoint());
    }

    #[test]
    fn classifies_fastapi_event_hooks() {
        let role = classify_decorator("app.on_event(\"startup\")").unwrap();
        match role {
            FrameworkRole::EventHandler { event, .. } => {
                assert_eq!(event, "startup");
            }
            other => panic!("expected EventHandler, got {other:?}"),
        }
    }

    #[test]
    fn pydantic_validators_are_entrypoints_but_not_data_classes() {
        let role = classify_decorator("validator(\"name\")").unwrap();
        assert!(role.is_framework_entrypoint());
        assert_eq!(role.family(), "pydantic_validator");
        let role = classify_decorator("dataclass").unwrap();
        assert!(!role.is_framework_entrypoint());
        assert_eq!(role.family(), "data_class");
    }

    #[test]
    fn pytest_fixture_does_not_match_framework_classifier() {
        // pytest fixtures are handled by the existing AST detection.
        // The framework classifier intentionally returns None for
        // them so dead-code does not double-count tests as routes.
        assert!(classify_decorator("pytest.fixture").is_none());
        assert!(classify_decorator("pytest.mark.parametrize(\"x\", [1])").is_none());
    }

    #[test]
    fn classify_decorators_picks_the_first_framework_match() {
        // Real code stacks @login_required above @router.get(...).
        // The first decorator (login_required) wins because Python
        // applies decorators bottom-up but our classifier keeps the
        // outermost framework as the canonical role.
        let stack = vec![
            "login_required".to_string(),
            "router.get(\"/me\")".to_string(),
        ];
        let role = classify_decorators(&stack).unwrap();
        match role {
            FrameworkRole::DjangoView { .. } => {}
            other => panic!("expected DjangoView wrapper, got {other:?}"),
        }
    }
}
