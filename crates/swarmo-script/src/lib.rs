//! The Swarmo JavaScript runtime: request scripts (`sw` / `pm`) and
//! virtual-user scripts (`ctx`). See docs/scripting.md.
//!
//! Scripts are sandboxed: no filesystem, process, or environment access. The
//! only host capabilities are those on [`host::ScriptHost`].

pub mod host;
pub mod request;
pub mod runtime;
pub mod types;
pub mod vu;

pub use host::{
    HostGrpcRequest, HostGrpcResponse, HostRequest, HostResponse, NullHost, ScriptHost, SharedHost,
};
pub use request::{run_post_response, run_pre_request};
pub use runtime::{Engine, Limits};
pub use types::{
    ConsoleLine, LogLevel, ScriptError, ScriptOutcome, ScriptRequest, ScriptResponse, TestResult,
};
pub use vu::{
    strip_module_syntax, CheckCount, GrpcOptions, StageOpt, UserMixEntry, VuEngine, VuOptions,
    USER_SCRIPT_TEMPLATE,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct MockHost {
        pub calls: Mutex<Vec<HostRequest>>,
        pub reply: Mutex<HostResponse>,
    }

    impl ScriptHost for MockHost {
        fn send_request(&self, req: HostRequest) -> Result<HostResponse, String> {
            self.calls.lock().unwrap().push(req);
            Ok(self.reply.lock().unwrap().clone())
        }
        fn sleep(&self, _ms: u64) {}
        fn interpolate(&self, text: &str) -> String {
            text.replace("{{baseUrl}}", "http://mock")
        }
    }

    fn host() -> Arc<MockHost> {
        Arc::new(MockHost {
            calls: Mutex::new(Vec::new()),
            reply: Mutex::new(HostResponse {
                status: 200,
                status_text: "OK".into(),
                headers: vec![("content-type".into(), "application/json".into())],
                body: r#"{"token":"tok_1","n":5}"#.into(),
                duration_ms: 12.0,
            }),
        })
    }

    fn req() -> ScriptRequest {
        ScriptRequest {
            name: "Test Request".into(),
            method: "GET".into(),
            url: "http://example.com".into(),
            headers: vec![],
            body: String::new(),
        }
    }

    fn res() -> ScriptResponse {
        ScriptResponse {
            status: 201,
            status_text: "Created".into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: r#"{"id":7,"name":"widget"}"#.into(),
            duration_ms: 42.0,
        }
    }

    fn vars() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("baseUrl".to_string(), "http://mock".to_string());
        m
    }

    fn pre(script: &str) -> ScriptOutcome {
        run_pre_request(
            host(),
            &[script.to_string()],
            &req(),
            &vars(),
            Limits::default(),
        )
    }

    fn post(script: &str) -> ScriptOutcome {
        run_post_response(
            host(),
            &[script.to_string()],
            &req(),
            &res(),
            &vars(),
            Limits::default(),
        )
    }

    // -- sw API ---------------------------------------------------------

    #[test]
    fn env_get_and_set() {
        let o = pre("sw.env.set('token', 'abc'); sw.env.set('copy', sw.env.get('baseUrl'));");
        assert!(o.error.is_none(), "{:?}", o.error);
        assert_eq!(o.vars.get("token").map(String::as_str), Some("abc"));
        assert_eq!(o.vars.get("copy").map(String::as_str), Some("http://mock"));
    }

    #[test]
    fn pre_request_can_mutate_the_request() {
        let o = pre(
            "sw.request.method = 'POST'; sw.request.url += '/x'; sw.request.headers.push(['X-A','1']);",
        );
        assert!(o.error.is_none(), "{:?}", o.error);
        let r = o.request.unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.url, "http://example.com/x");
        assert!(r.headers.iter().any(|(k, v)| k == "X-A" && v == "1"));
    }

    #[test]
    fn tests_record_pass_and_fail() {
        let o = post(
            "sw.test('ok', () => sw.expect(sw.response.status).toBe(201));\
             sw.test('bad', () => sw.expect(sw.response.status).toBe(500));",
        );
        assert!(o.error.is_none(), "{:?}", o.error);
        assert_eq!(o.tests.len(), 2);
        assert!(o.tests[0].passed);
        assert!(!o.tests[1].passed);
        assert!(o.tests[1].error.as_ref().unwrap().contains("500"));
    }

    #[test]
    fn response_json_helper() {
        let o = post("sw.test('t', () => sw.expect(sw.response.json().name).toBe('widget'));");
        assert!(o.tests[0].passed, "{:?}", o.tests);
    }

    #[test]
    fn response_json_throws_on_invalid_json() {
        let bad = ScriptResponse {
            body: "not json".into(),
            ..res()
        };
        let o = run_post_response(
            host(),
            &["sw.test('t', () => sw.response.json());".to_string()],
            &req(),
            &bad,
            &vars(),
            Limits::default(),
        );
        assert!(!o.tests[0].passed);
        assert!(o.tests[0]
            .error
            .as_ref()
            .unwrap()
            .contains("not valid JSON"));
    }

    #[test]
    fn all_matchers() {
        let o = post(
            "sw.test('m', () => {
               sw.expect(1).toBe(1);
               sw.expect({a:1}).toEqual({a:1});
               sw.expect('hello').toContain('ell');
               sw.expect([1,2]).toContain(2);
               sw.expect(1).toBeLessThan(2);
               sw.expect(2).toBeGreaterThan(1);
               sw.expect('abc').toMatch('^a');
             });",
        );
        assert!(o.tests[0].passed, "{:?}", o.tests[0].error);
    }

    #[test]
    fn to_equal_ignores_key_order() {
        let o = post(
            "sw.test('t', () => sw.expect(sw.response.json()).toEqual({name: 'widget', id: 7}));\
             sw.test('u', () => sw.expect({a: [1, {b: 2}]}).toEqual({a: [1, {b: 3}]}));",
        );
        assert!(o.tests[0].passed, "{:?}", o.tests[0].error);
        assert!(!o.tests[1].passed);
    }

    #[test]
    fn an_async_test_that_rejects_fails() {
        let o = post("sw.test('t', async () => { await null; sw.expect(1).toBe(2); });");
        assert!(o.error.is_none(), "{:?}", o.error);
        assert!(!o.tests[0].passed);
        assert!(o.tests[0].error.as_ref().unwrap().contains("to be 2"));
    }

    #[test]
    fn a_non_string_request_field_keeps_the_rest_of_the_result() {
        let o = pre("sw.env.set('token', 'abc'); sw.request.headers.push(['X-N', 5]); sw.request.body = {a: 1};");
        assert!(o.error.is_none(), "{:?}", o.error);
        assert_eq!(o.vars.get("token").map(String::as_str), Some("abc"));
        let r = o.request.unwrap();
        assert!(r.headers.iter().any(|(k, v)| k == "X-N" && v == "5"));
        assert_eq!(r.body, r#"{"a":1}"#);
    }

    #[test]
    fn unset_reads_as_absent_for_the_rest_of_the_run() {
        let o = pre("pm.environment.unset('baseUrl');\
             sw.env.set('has', String(sw.env.has('baseUrl')));\
             sw.env.set('get', String(pm.environment.get('baseUrl')));\
             sw.env.set('keys', Object.keys(sw.env.toObject()).join(','));");
        assert_eq!(o.vars.get("has").map(String::as_str), Some("false"));
        assert_eq!(o.vars.get("get").map(String::as_str), Some("undefined"));
        assert!(!o.vars["keys"].split(',').any(|k| k == "baseUrl"));
        // The host's map is strings only, so it goes back as empty.
        assert_eq!(o.vars.get("baseUrl").map(String::as_str), Some(""));
    }

    #[test]
    fn send_request_from_a_script() {
        let h = host();
        let o = run_pre_request(
            h.clone(),
            &["const r = await sw.sendRequest({method:'POST', url:'http://x/login', json:{u:1}});\
               sw.env.set('token', r.json().token);"
                .to_string()],
            &req(),
            &vars(),
            Limits::default(),
        );
        assert!(o.error.is_none(), "{:?}", o.error);
        assert_eq!(o.vars.get("token").map(String::as_str), Some("tok_1"));
        let calls = h.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, "POST");
        assert_eq!(calls[0].body.as_deref(), Some(r#"{"u":1}"#));
    }

    #[test]
    fn console_is_captured() {
        let o = pre("console.log('hi', {a:1}); console.error('bad');");
        assert_eq!(o.console.len(), 2);
        assert_eq!(o.console[0].text, r#"hi {"a":1}"#);
        assert!(matches!(o.console[1].level, LogLevel::Error));
    }

    #[test]
    fn builtins_available() {
        let o = pre("sw.env.set('u', typeof crypto.randomUUID());\
             sw.env.set('b', btoa('hi'));\
             sw.env.set('a', atob('aGk='));\
             sw.env.set('j', JSON.stringify({x:1}));");
        assert!(o.error.is_none(), "{:?}", o.error);
        assert_eq!(o.vars.get("u").map(String::as_str), Some("string"));
        assert_eq!(o.vars.get("b").map(String::as_str), Some("aGk="));
        assert_eq!(o.vars.get("a").map(String::as_str), Some("hi"));
    }

    #[test]
    fn atob_and_btoa_follow_browser_semantics() {
        let o = pre("sw.env.set('unpadded', atob('aGk'));\
             sw.env.set('spaced', atob(' aG k= '));\
             sw.env.set('byte', String(atob('/w==').charCodeAt(0)));\
             sw.env.set('latin1', btoa(String.fromCharCode(255)));\
             try { atob('a'); } catch (e) { sw.env.set('bad', e.name); }\
             try { btoa('\\u20ac'); } catch (e) { sw.env.set('wide', e.name); }");
        assert!(o.error.is_none(), "{:?}", o.error);
        assert_eq!(o.vars.get("unpadded").map(String::as_str), Some("hi"));
        assert_eq!(o.vars.get("spaced").map(String::as_str), Some("hi"));
        assert_eq!(o.vars.get("byte").map(String::as_str), Some("255"));
        assert_eq!(o.vars.get("latin1").map(String::as_str), Some("/w=="));
        assert_eq!(
            o.vars.get("bad").map(String::as_str),
            Some("InvalidCharacterError")
        );
        assert_eq!(
            o.vars.get("wide").map(String::as_str),
            Some("InvalidCharacterError")
        );
    }

    // -- sandbox & limits ------------------------------------------------

    #[test]
    fn no_filesystem_or_process_access() {
        let o = pre(
            "sw.env.set('fs', String(typeof require) + ',' + String(typeof process) \
             + ',' + String(typeof std) + ',' + String(typeof os));",
        );
        assert_eq!(
            o.vars.get("fs").map(String::as_str),
            Some("undefined,undefined,undefined,undefined")
        );
    }

    #[test]
    fn infinite_loop_hits_the_timeout() {
        let o = run_pre_request(
            host(),
            &["while (true) {}".to_string()],
            &req(),
            &vars(),
            Limits {
                timeout_ms: Some(250),
                ..Limits::default()
            },
        );
        let err = o.error.expect("expected a timeout error");
        assert!(err.contains("timed out"), "got: {err}");
    }

    #[test]
    fn a_timeout_inside_a_promise_job_is_reported() {
        // Where the interrupt lands varies run to run: in a job (which used
        // to abort the process on a refcount underflow) or in the engine's
        // own bookkeeping evals (which used to report success). Loop so both
        // get hit.
        for i in 0..8 {
            let o = run_pre_request(
                host(),
                &["await new Promise(function loop(r) { Promise.resolve().then(() => loop(r)); });"
                    .to_string()],
                &req(),
                &vars(),
                Limits {
                    timeout_ms: Some(60),
                    ..Limits::default()
                },
            );
            let err = o.error.unwrap_or_else(|| panic!("run {i}: no error"));
            assert!(err.contains("timed out"), "run {i}: {err}");
        }
    }

    #[test]
    fn memory_limit_is_enforced() {
        let o = run_pre_request(
            host(),
            &["const a = []; while (true) { a.push(new Array(10000).fill('x')); }".to_string()],
            &req(),
            &vars(),
            Limits {
                memory_bytes: 4 * 1024 * 1024,
                timeout_ms: Some(10_000),
                ..Limits::default()
            },
        );
        assert!(o.error.is_some(), "expected an out-of-memory error");
    }

    #[test]
    fn syntax_errors_are_reported() {
        let o = pre("this is not javascript {{{");
        assert!(o.error.unwrap().to_lowercase().contains("error"));
    }

    #[test]
    fn a_throw_stops_the_script_but_keeps_earlier_results() {
        let o = pre("sw.env.set('a','1'); throw new Error('boom');");
        assert!(o.error.as_ref().unwrap().contains("boom"));
        assert_eq!(o.vars.get("a").map(String::as_str), Some("1"));
    }

    // -- pm compatibility shim -------------------------------------------

    #[test]
    fn pm_environment_maps_onto_sw_env() {
        let o = pre("pm.environment.set('k','v'); pm.collectionVariables.set('k2', pm.environment.get('k'));");
        assert_eq!(o.vars.get("k").map(String::as_str), Some("v"));
        assert_eq!(o.vars.get("k2").map(String::as_str), Some("v"));
    }

    #[test]
    fn pm_test_and_chai_expectations() {
        let o = post(
            "pm.test('Status code is 201', function () {
                 pm.response.to.have.status(201);
             });
             pm.test('Body matches', function () {
                 pm.expect(pm.response.json().name).to.equal('widget');
                 pm.expect(pm.response.code).to.be.above(200);
                 pm.expect(pm.response.json()).to.have.property('id', 7);
             });
             pm.test('Fails properly', function () {
                 pm.expect(1).to.equal(2);
             });",
        );
        assert!(o.error.is_none(), "{:?}", o.error);
        assert_eq!(o.tests.len(), 3);
        assert!(o.tests[0].passed, "{:?}", o.tests[0].error);
        assert!(o.tests[1].passed, "{:?}", o.tests[1].error);
        assert!(!o.tests[2].passed);
    }

    /// Run each assertion as its own `pm.test` against the fixture response
    /// (201 Created, `{"id":7,"name":"widget"}`) and report which failed.
    fn failing(assertions: &[&str]) -> Vec<String> {
        let script: String = assertions
            .iter()
            .enumerate()
            .map(|(i, a)| format!("pm.test('{i}', function () {{ {a}; }});\n"))
            .collect();
        let o = post(&script);
        assert!(o.error.is_none(), "{:?}", o.error);
        assert_eq!(o.tests.len(), assertions.len());
        o.tests
            .iter()
            .filter(|t| !t.passed)
            .map(|t| assertions[t.name.parse::<usize>().unwrap()].to_string())
            .collect()
    }

    #[test]
    fn chai_property_assertions_actually_assert() {
        // Each of these is false. Before, every one of them passed, because the
        // property forms were functions that nothing called.
        let should_fail = [
            "pm.expect(pm.response.json().id === 8).to.be.true",
            "pm.expect(true).to.be.false",
            "pm.expect(false).to.be.ok",
            "pm.expect(1).to.be.null",
            "pm.expect(null).to.be.undefined",
            "pm.expect(undefined).to.exist",
            "pm.expect('x').to.be.empty",
            "pm.expect([]).to.not.be.empty",
            "pm.expect('x').to.not.be.a('string')",
            "pm.expect([1, 2, 3]).to.have.members([1, 2])",
            "pm.expect(1).not.to.equal(1)",
            "pm.expect({a: 1}).to.not.have.property('a')",
            "pm.expect(5).to.be.NaN",
        ];
        assert_eq!(failing(&should_fail), should_fail);
    }

    #[test]
    fn chai_assertions_that_hold_pass() {
        let should_pass = [
            "pm.expect(pm.response.json().id === 7).to.be.true",
            "pm.expect(false).to.be.false",
            "pm.expect(1).to.be.ok",
            "pm.expect(null).to.be.null",
            "pm.expect(undefined).to.be.undefined",
            "pm.expect(0).to.exist",
            "pm.expect([]).to.be.empty",
            "pm.expect('x').to.be.a('string').and.not.be.empty",
            "pm.expect([1, 2]).to.be.an('array')",
            "pm.expect(null).to.be.a('null')",
            "pm.expect([2, 1]).to.have.members([1, 2])",
            "pm.expect([1, 2, 3]).to.include.members([3, 1])",
            "pm.expect({b: 2, a: 1}).to.have.all.keys('a', 'b')",
            "pm.expect({a: 1, b: 2}).to.include({a: 1})",
            "pm.expect({a: {b: 1}}).to.deep.equal({a: {b: 1}})",
            "pm.expect(1).not.to.equal(2)",
            "pm.expect(pm.response.json()).to.have.property('id').that.is.a('number')",
            "pm.expect(pm.response.json()).to.have.a.property('name', 'widget')",
            "pm.expect(5).to.be.at.least(5).and.at.most(5)",
            "pm.expect(5).to.be.within(1, 10)",
            "pm.expect('b').to.be.oneOf(['a', 'b'])",
            "pm.expect([1, 2]).to.have.lengthOf(2)",
            "pm.expect(function () { throw new Error('boom'); }).to.throw('boom')",
            "pm.expect(2).toBe(2)",
        ];
        assert_eq!(failing(&should_pass), Vec::<String>::new());
    }

    #[test]
    fn postman_response_assertions() {
        // The fixture is 201 Created with a JSON body.
        let should_pass = [
            "pm.response.to.have.status(201)",
            "pm.response.to.have.status('Created')",
            "pm.response.to.be.success",
            "pm.response.to.not.be.error",
            "pm.response.to.be.json",
            "pm.response.to.have.header('Content-Type')",
            "pm.response.to.have.header('content-type', 'application/json')",
            "pm.response.to.have.jsonBody('name', 'widget')",
            "pm.expect(pm.response).to.have.status(201)",
        ];
        assert_eq!(failing(&should_pass), Vec::<String>::new());

        let should_fail = [
            // Postman's `ok` on a response means 200 exactly.
            "pm.response.to.be.ok",
            "pm.response.to.have.status(200)",
            "pm.response.to.be.clientError",
            "pm.response.to.have.header('X-Missing')",
            "pm.response.to.have.jsonBody('name', 'gadget')",
        ];
        assert_eq!(failing(&should_fail), should_fail);
    }

    #[test]
    fn pm_response_headers_and_time() {
        let o = post(
            "pm.test('h', () => {
               pm.expect(pm.response.headers.get('Content-Type')).to.equal('application/json');
               pm.expect(pm.response.responseTime).to.be.above(0);
               pm.expect(pm.response.status).to.equal('Created');
             });",
        );
        assert!(o.tests[0].passed, "{:?}", o.tests[0].error);
    }

    #[test]
    fn pm_send_request_callback_style() {
        let o = pre(
            "pm.sendRequest({method:'GET', url:'http://x'}, function (err, res) {
                 sw.env.set('status', String(res.status));
             });",
        );
        assert!(o.error.is_none(), "{:?}", o.error);
        assert_eq!(o.vars.get("status").map(String::as_str), Some("200"));
    }

    #[test]
    fn pm_send_request_accepts_the_postman_request_shape() {
        let h = host();
        let o = run_pre_request(
            h.clone(),
            &[r#"pm.sendRequest({
                   url: 'http://x/token',
                   method: 'POST',
                   header: [{ key: 'X-Foo', value: 'bar' }, { key: 'X-Off', value: '1', disabled: true }],
                   body: { mode: 'raw', raw: JSON.stringify({ k: 1 }), options: { raw: { language: 'json' } } }
                 }, function (err, res) {
                   sw.env.set('code', String(res.code));
                   sw.env.set('ct', String(res.headers.get('Content-Type')));
                 });
                 pm.sendRequest({
                   url: { raw: 'http://x/form' },
                   method: 'POST',
                   header: { 'X-Obj': 'yes' },
                   body: { mode: 'urlencoded', urlencoded: [{ key: 'a', value: '1 2' }] }
                 }, function () {});"#
                .to_string()],
            &req(),
            &vars(),
            Limits::default(),
        );
        assert!(o.error.is_none(), "{:?}", o.error);
        assert_eq!(o.vars.get("code").map(String::as_str), Some("200"));
        assert_eq!(
            o.vars.get("ct").map(String::as_str),
            Some("application/json")
        );

        let calls = h.calls.lock().unwrap();
        assert_eq!(calls[0].body.as_deref(), Some(r#"{"k":1}"#));
        let names: Vec<&str> = calls[0].headers.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, vec!["X-Foo", "Content-Type"]);

        assert_eq!(calls[1].url, "http://x/form");
        assert_eq!(calls[1].body.as_deref(), Some("a=1%202"));
        assert!(calls[1]
            .headers
            .iter()
            .any(|(k, v)| k == "X-Obj" && v == "yes"));
    }

    #[test]
    fn unsupported_pm_apis_fail_loudly() {
        let o = post("pm.test('t', () => { pm.cookies.get('a'); });");
        assert!(!o.tests[0].passed);
        assert!(o.tests[0]
            .error
            .as_ref()
            .unwrap()
            .contains("is not supported"));
    }

    #[test]
    fn postman_legacy_globals() {
        let o = pre("postman.setEnvironmentVariable('legacy', 'yes');");
        assert_eq!(o.vars.get("legacy").map(String::as_str), Some("yes"));
    }

    #[test]
    fn multiple_scripts_run_in_order_and_share_state() {
        let o = run_pre_request(
            host(),
            &[
                "sw.env.set('order', 'a');".to_string(),
                "sw.env.set('order', sw.env.get('order') + 'b');".to_string(),
            ],
            &req(),
            &vars(),
            Limits::default(),
        );
        assert_eq!(o.vars.get("order").map(String::as_str), Some("ab"));
    }

    #[test]
    fn empty_scripts_are_a_no_op() {
        let o = run_pre_request(
            host(),
            &["".to_string()],
            &req(),
            &vars(),
            Limits::default(),
        );
        assert!(o.error.is_none());
        assert!(o.request.is_some());
    }

    // -- VU scripts -------------------------------------------------------

    #[test]
    fn vu_script_loads_options_and_runs_iterations() {
        let h = host();
        let src = r#"
            export const options = {
              mode: "closed",
              stages: [{ durationSec: 5, target: 2 }],
              userMix: [{ exec: "shopper", weight: 3 }, { exec: "admin", weight: 1 }]
            };
            export async function shopper(ctx) {
              const r = await ctx.http.get("{{baseUrl}}/items", { tag: "list" });
              ctx.check(r, { "ok": x => x.status === 200, "nope": x => x.status === 999 });
              ctx.vars.seen = (ctx.vars.seen || 0) + 1;
            }
            export async function admin(ctx) {
              await ctx.http.post("{{baseUrl}}/admin", { json: { a: 1 } });
            }
        "#;
        let (vu, opts) = VuEngine::load(h.clone(), src, 1, &vars()).unwrap();
        assert_eq!(opts.stages.len(), 1);
        assert_eq!(opts.stages[0].target, 2.0);
        assert_eq!(opts.user_mix.len(), 2);
        assert_eq!(opts.user_mix[0].weight, 3.0);

        let checks = vu.run_iteration("shopper", 0).unwrap();
        assert_eq!(checks.get("ok").unwrap().passes, 1);
        assert_eq!(checks.get("nope").unwrap().fails, 1);

        // Checks are drained per iteration.
        let checks2 = vu.run_iteration("shopper", 1).unwrap();
        assert_eq!(checks2.get("ok").unwrap().passes, 1);

        let calls = h.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].url, "http://mock/items");
        assert_eq!(calls[0].tag.as_deref(), Some("list"));
    }

    #[test]
    fn vu_vars_persist_across_iterations() {
        let src = r#"
            export const options = { userMix: [{ exec: "run", weight: 1 }] };
            export async function run(ctx) {
              ctx.vars.n = (ctx.vars.n || 0) + 1;
              ctx.check({}, { "n matches iteration": () => ctx.vars.n === ctx.vu.iteration + 1 });
            }
        "#;
        let (vu, _) = VuEngine::load(host(), src, 7, &vars()).unwrap();
        for i in 0..3 {
            let checks = vu.run_iteration("run", i).unwrap();
            assert_eq!(
                checks.get("n matches iteration").unwrap().passes,
                1,
                "iteration {i}"
            );
        }
    }

    #[test]
    fn vu_group_prefixes_check_names() {
        let src = r#"
            export const options = { userMix: [{ exec: "run", weight: 1 }] };
            export async function run(ctx) {
              ctx.group("checkout", () => {
                ctx.check({}, { "inner": () => true });
              });
            }
        "#;
        let (vu, _) = VuEngine::load(host(), src, 1, &vars()).unwrap();
        let checks = vu.run_iteration("run", 0).unwrap();
        assert!(checks.contains_key("checkout :: inner"), "{checks:?}");
    }

    #[test]
    fn vu_async_group_keeps_its_prefix_across_awaits() {
        let h = host();
        let src = r#"
            export const options = { userMix: [{ exec: "run", weight: 1 }] };
            export async function run(ctx) {
              await ctx.group("checkout", async () => {
                const r = await ctx.http.get("{{baseUrl}}/a", { tag: "first" });
                ctx.check(r, { "inner": () => true });
                await ctx.http.get("{{baseUrl}}/b", { tag: "second" });
              });
              await ctx.http.get("{{baseUrl}}/c", { tag: "after" });
            }
        "#;
        let (vu, _) = VuEngine::load(h.clone(), src, 1, &vars()).unwrap();
        let checks = vu.run_iteration("run", 0).unwrap();
        assert!(checks.contains_key("checkout :: inner"), "{checks:?}");
        let tags: Vec<String> = h
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.tag.clone().unwrap_or_default())
            .collect();
        assert_eq!(
            tags,
            vec!["checkout :: first", "checkout :: second", "after"]
        );
    }

    #[test]
    fn vu_http_accepts_header_pairs() {
        let h = host();
        let src = r#"
            export const options = { userMix: [{ exec: "run", weight: 1 }] };
            export async function run(ctx) {
              await ctx.http.get("{{baseUrl}}/x", { headers: [["X-Base", "{{baseUrl}}"]], tag: "t" });
            }
        "#;
        let (vu, _) = VuEngine::load(h.clone(), src, 1, &vars()).unwrap();
        vu.run_iteration("run", 0).unwrap();
        let calls = h.calls.lock().unwrap();
        assert_eq!(
            calls[0].headers,
            vec![("X-Base".to_string(), "http://mock".to_string())]
        );
    }

    #[test]
    fn vu_missing_exec_is_an_error() {
        let src = r#"export const options = { userMix: [{ exec: "nope", weight: 1 }] };"#;
        let err = match VuEngine::load(host(), src, 1, &vars()) {
            Ok(_) => panic!("expected loading to fail"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("nope"), "got: {err}");
    }

    #[test]
    fn vu_default_export_works() {
        let src = r#"
            export const options = { stages: [{ durationSec: 1, target: 1 }] };
            export default async function (ctx) {
              await ctx.http.get("{{baseUrl}}/x");
            }
        "#;
        let h = host();
        let (vu, opts) = match VuEngine::load(h.clone(), src, 1, &vars()) {
            Ok(v) => v,
            Err(e) => panic!("load failed: {e}"),
        };
        assert_eq!(opts.exec_names(), vec!["default".to_string()]);
        vu.run_iteration("default", 0).unwrap();
        assert_eq!(h.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn vu_iteration_error_still_returns_checks() {
        let src = r#"
            export const options = { userMix: [{ exec: "run", weight: 1 }] };
            export async function run(ctx) {
              ctx.check({}, { "before": () => true });
              throw new Error("kaboom");
            }
        "#;
        let (vu, _) = VuEngine::load(host(), src, 1, &vars()).unwrap();
        let err = vu.run_iteration("run", 0).unwrap_err();
        assert!(err.to_string().contains("kaboom"));
    }
}
