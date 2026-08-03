#!/usr/bin/env node
// RFC-0107 Phase 1: executable Program + typed event decoder + subscription
// reconciliation over the compatibility JSON host.

import { execFileSync } from "node:child_process";
import {
  copyFileSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { mount } from "./glamour-dom.mjs";
import { createHostSimulator } from "./glamour-host-simulator.mjs";
import { createReferenceDom } from "./glamour-reference-dom.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const BIN = process.argv[2]
  ? resolve(process.cwd(), process.argv[2])
  : resolve(REPO, "target/debug/witchy");
const work = mkdtempSync(join(tmpdir(), "glamour-program-"));

const SOURCE = `import reflect
from glamour import Cmd, CredentialPort, HttpResult, NavigationResult, PortResult, Program, SecretInput, Start, Sub, Ui, UiFetch, UiRoot, UiRoute, UiTimer
from json import Json

type Msg derive(Reflect):
    Tick
    CancelDelay
    StartHttp
    GotHttp(HttpResult)
    StartPort
    GotPort(PortResult)
    StartNavigation
    GotNavigation(NavigationResult)
    StartMappedHttp
    Child(ChildMsg)
    SecretReady(String)
    StartSecret
    GotSecret(PortResult)
    StartMalformedHttp

type ChildMsg derive(Reflect):
    ChildHttp(HttpResult)

fn lift(message: ChildMsg) -> Msg:
    Child(message)

fn child_http(fetch: UiFetch) -> Cmd(ChildMsg):
    glamour.http_get("counter.mapped-http", fetch, "/typed", fn(result: HttpResult): ChildHttp(result))

type Auth:
    Auth(UiTimer, UiFetch, UiRoute, CredentialPort, SecretInput, CredentialPort)

fn authorize(root: UiRoot) -> Auth:
    Auth(
        glamour.timer_scope(root, 10),
        glamour.fetch_scope(root, "test", "GET", "/"),
        glamour.route_scope(root, "/", "push"),
        glamour.credential_port(root, "echo"),
        glamour.secret_field(root, "test", "password"),
        glamour.credential_port(root, "secretEcho"),
    )

fn initial(_start: Start) -> Int:
    0

fn start(auth: Auth, _model: Int) -> Cmd(Msg):
    match auth:
        Auth(timer, _fetch, _route, _port, _secret, _secret_port) -> glamour.schedule("counter.delay", timer, 10, Tick)

fn update(auth: Auth, model: Int, message: Msg) -> (Int, Cmd(Msg)):
    match auth:
        Auth(timer, fetch, route, port, secret, secret_port) ->
            match message:
                Tick -> (model + 1, glamour.schedule("counter.delay", timer, 10, Tick))
                CancelDelay -> (model, glamour.cancel_cmd("counter.delay"))
                StartHttp -> (model, glamour.http_get("counter.http", fetch, "/typed", fn(result: HttpResult): GotHttp(result)))
                GotHttp(result) ->
                    match result:
                        HttpResponse(status, body) -> (model + status + body.length(), NoCmd)
                        HttpFailure(_problem) -> (model - 100, NoCmd)
                        _ -> (model, NoCmd)
                StartPort -> (model, glamour.port("counter.port", port, "typed", fn(result: PortResult): GotPort(result)))
                GotPort(result) ->
                    match result:
                        PortResponse(value) -> (model + value.length(), NoCmd)
                        PortFailure(_problem) -> (model - 1000, NoCmd)
                        _ -> (model, NoCmd)
                StartNavigation -> (model, glamour.navigate("counter.nav", route, "/next", fn(result: NavigationResult): GotNavigation(result)))
                GotNavigation(result) ->
                    match result:
                        Navigated(path) -> (model + path.length(), NoCmd)
                        _ -> (model, NoCmd)
                StartMappedHttp -> (model, child_http(fetch).map(lift))
                Child(ChildHttp(result)) ->
                    match result:
                        HttpResponse(status, body) -> (model + 1000 + status + body.length(), NoCmd)
                        HttpFailure(_problem) -> (model - 10000, NoCmd)
                        _ -> (model, NoCmd)
                SecretReady(_status) -> (model, NoCmd)
                StartSecret -> (model, glamour.submit_secret("counter.secret", glamour.secret_ref(secret), secret_port, fn(result: PortResult): GotSecret(result)))
                GotSecret(result) ->
                    match result:
                        PortResponse(value) -> (model + value.length(), NoCmd)
                        PortFailure(_problem) -> (model - 100000, NoCmd)
                        _ -> (model, NoCmd)
                StartMalformedHttp -> (model, glamour.http_get("counter.malformed", fetch, "/typed", fn(_result: HttpResult): Tick))

fn render(model: Int) -> Ui(Msg):
    glamour.ui(glamour.element("div", [], [
        glamour.element("button", [glamour.on_event("counter.tick", "click", glamour.event_msg(Tick))], [glamour.text("tick \${model}")]),
        glamour.element("button", [glamour.on_event("counter.cancel", "click", glamour.event_msg(CancelDelay))], [glamour.text("cancel")]),
        glamour.element("button", [glamour.on_event("counter.http", "click", glamour.event_msg(StartHttp))], [glamour.text("http")]),
        glamour.element("button", [glamour.on_event("counter.port", "click", glamour.event_msg(StartPort))], [glamour.text("port")]),
        glamour.element("button", [glamour.on_event("counter.nav", "click", glamour.event_msg(StartNavigation))], [glamour.text("nav")]),
        glamour.element("button", [glamour.on_event("counter.mapped-http", "click", glamour.event_msg(StartMappedHttp))], [glamour.text("mapped http")]),
        glamour.element("button", [glamour.on_event("counter.malformed", "click", glamour.event_msg(StartMalformedHttp))], [glamour.text("malformed")]),
    ]))

fn subscriptions(auth: Auth, model: Int) -> Sub(Msg):
    match auth:
        Auth(timer, _fetch, _route, _port, _secret, _secret_port) ->
            let milliseconds = if model == 0: 10 else: 20
            glamour.every("counter.clock", timer, milliseconds, Tick)

fn parse_model(document: Json) -> Int:
    json.as_int(document) ?? 0

fn first_value(document: Json) -> Json:
    match json.get(document, "\\$values"):
        Some(values) -> json.index(values, 0) ?? JsonNull
        None -> JsonNull

fn value_int(values: Json, index: Int) -> Int:
    match json.index(values, index):
        Some(value) -> json.as_int(value) ?? 0
        None -> 0

fn value_string(values: Json, index: Int) -> String:
    match json.index(values, index):
        Some(value) -> json.as_string(value) ?? ""
        None -> ""

fn parse_http(document: Json) -> HttpResult:
    match json.get_string(document, "\\$variant"):
        Some("HttpResponse") ->
            let values = json.get(document, "\\$values") ?? JsonArray([])
            HttpResponse(value_int(values, 0), value_string(values, 1))
        Some("HttpFailure") ->
            let values = json.get(document, "\\$values") ?? JsonArray([])
            HttpFailure(value_string(values, 0))
        _ -> HttpFailure("invalid")

fn parse_port(document: Json) -> PortResult:
    match json.get_string(document, "\\$variant"):
        Some("PortResponse") ->
            let values = json.get(document, "\\$values") ?? JsonArray([])
            PortResponse(value_string(values, 0))
        Some("PortFailure") ->
            let values = json.get(document, "\\$values") ?? JsonArray([])
            PortFailure(value_string(values, 0))
        _ -> PortFailure("invalid")

fn parse_navigation(document: Json) -> NavigationResult:
    let values = json.get(document, "\\$values") ?? JsonArray([])
    Navigated(value_string(values, 0))

fn parse_child(document: Json) -> ChildMsg:
    ChildHttp(parse_http(first_value(document)))

fn parse_msg(document: Json) -> Msg:
    match json.get_string(document, "\\$variant"):
        Some("GotHttp") -> GotHttp(parse_http(first_value(document)))
        Some("GotPort") -> GotPort(parse_port(first_value(document)))
        Some("GotNavigation") -> GotNavigation(parse_navigation(first_value(document)))
        Some("Child") -> Child(parse_child(first_value(document)))
        Some("SecretReady") -> SecretReady(value_string(json.get(document, "\\$values") ?? JsonArray([]), 0))
        Some("GotSecret") -> GotSecret(parse_port(first_value(document)))
        _ -> Tick

fn model_to_json(model: Int) -> Json:
    json.from_value(model)

fn msg_to_json(message: Msg) -> Json:
    json.from_value(message)

fn app() -> Program(Auth, Int, Msg):
    glamour.program(authorize, initial, start, update, render, subscriptions)

pub fn export_step(root: UiRoot, input: String) -> String:
    glamour.program_step_with(input, root, app(), parse_model, parse_msg, model_to_json, msg_to_json)

fn main(console: Console):
    console.print("program")
`;

let failures = 0;
const ok = (condition, message) => {
  console.log(`  ${condition ? "ok" : "FAIL"}: ${message}`);
  if (!condition) failures += 1;
};

try {
  copyFileSync(
    join(REPO, "projects/glamour/src/glamour.witchy"),
    join(work, "glamour.witchy"),
  );
  writeFileSync(join(work, "program.witchy"), SOURCE);
  const wasmPath = join(work, "program.wasm");
  execFileSync(BIN, ["compile", "program.witchy", "--out", wasmPath], {
    cwd: work,
    stdio: "pipe",
  });

  const dom = createReferenceDom();
  const root = dom.createRoot();
  const simulator = createHostSimulator();
  const navigations = [];
  const fetches = [];
  const app = await mount(readFileSync(wasmPath), root, {
    document: dom.document,
    start: { route: "/", bootstrap: "" },
    instantiateOpts: { userCaps: [["app"]] },
    fetch: () =>
      new Promise((resolveFetch) => {
        fetches.push({ resolve: resolveFetch });
      }),
    ports: {
      echo: async (argument) => `echo:${argument}`,
    },
    history: {
      pushState: (_state, _title, path) => navigations.push(path),
    },
    ...simulator.timerOptions,
  });

  ok(app.getModel() === 0, "Program.initial owns the initial model");
  ok(app.getRuntimeStats().activeStableCommands === 1, "Program.start runs one authorized startup command");
  ok(simulator.pending("timeout").length === 1, "the startup command is live after activation");
  ok(app.getRuntimeStats().activeSubscriptions === 1, "one stable subscription is active");
  ok(simulator.pending("interval").length === 1, "the host starts one repeating source");

  const [
    button,
    cancelButton,
    httpButton,
    portButton,
    navigationButton,
    mappedHttpButton,
    malformedHttpButton,
  ] =
    dom.findAll(root, "button");
  const firstInterval = simulator.pending("interval")[0];
  const staleIntervalCallback = simulator.callbackOf(firstInterval.handle);
  const startupTimeout = simulator.pending("timeout")[0];
  const staleStartupCallback = simulator.callbackOf(startupTimeout.handle);
  button.dispatchEvent({ type: "click", target: button });
  ok(app.getModel() === 1, "a typed decoder ID dispatches its Witchy message");
  const secondInterval = simulator.pending("interval")[0];
  ok(secondInterval.handle !== firstInterval.handle, "a changed subscription replaces its host handle");
  ok(simulator.pending("timeout").length === 1, "a stable delayed command starts once");
  const firstTimeout = simulator.pending("timeout")[0];
  const staleTimeoutCallback = simulator.callbackOf(firstTimeout.handle);
  staleStartupCallback();
  ok(app.getModel() === 1, "a replaced startup command generation is ignored");
  button.dispatchEvent({ type: "click", target: button });
  const secondTimeout = simulator.pending("timeout")[0];
  ok(app.getModel() === 2 && secondTimeout.handle !== firstTimeout.handle, "re-emitting a command ID replaces its generation");
  staleTimeoutCallback();
  ok(app.getModel() === 2, "a stale command generation is ignored");
  const cancelledTimeoutCallback = simulator.callbackOf(secondTimeout.handle);
  cancelButton.dispatchEvent({ type: "click", target: cancelButton });
  ok(simulator.pending("timeout").length === 0 && app.getRuntimeStats().activeStableCommands === 0, "CancelCmd removes the active generation");
  cancelledTimeoutCallback();
  ok(app.getModel() === 2, "a callback queued before explicit cancellation is ignored");
  staleIntervalCallback();
  ok(app.getModel() === 2, "a stale cancelled subscription generation is ignored");

  const intervalCallback = simulator.callbackOf(secondInterval.handle);
  intervalCallback();
  ok(app.getModel() === 3, "the repeating subscription dispatches its typed message");
  ok(simulator.pending("interval")[0].handle === secondInterval.handle, "an unchanged subscription keeps its host handle");

  const settle = async () => {
    for (let index = 0; index < 8; index += 1) await Promise.resolve();
  };
  const stableBeforeHttp = app.getRuntimeStats().activeStableCommands;
  httpButton.dispatchEvent({ type: "click", target: httpButton });
  ok(app.getRuntimeStats().activeStableCommands === stableBeforeHttp + 1, "a typed HTTP command has stable in-flight identity");
  httpButton.dispatchEvent({ type: "click", target: httpButton });
  ok(fetches.length === 2, "re-emitting a typed HTTP ID starts a replacement generation");
  fetches[0].resolve({ status: 500, text: async () => "stale" });
  await settle();
  ok(app.getModel() === 3, "a stale typed HTTP completion is ignored");
  fetches[1].resolve({ status: 201, text: async () => "typed-body" });
  await settle();
  ok(app.getModel() === 214, "a typed HTTP completion reaches its Witchy constructor");
  ok(app.getRuntimeStats().activeStableCommands === stableBeforeHttp, "HTTP completion retires its generation");

  portButton.dispatchEvent({ type: "click", target: portButton });
  await settle();
  ok(app.getModel() === 224, "a typed port completion reaches its Witchy constructor");

  navigationButton.dispatchEvent({ type: "click", target: navigationButton });
  ok(navigations.join(",") === "/next", "typed navigation performs the authorized history write");
  ok(app.getModel() === 229, "typed navigation completes without a global route tag");

  mappedHttpButton.dispatchEvent({ type: "click", target: mappedHttpButton });
  fetches[2].resolve({ status: 201, text: async () => "typed-body" });
  await settle();
  ok(app.getModel() === 1440, "Cmd.map preserves a nested typed completion callback");

  const fetchCountBeforeMalformed = fetches.length;
  let malformedRejected = false;
  try {
    malformedHttpButton.dispatchEvent({ type: "click", target: malformedHttpButton });
  } catch (error) {
    malformedRejected = String(error).includes("exactly one result slot");
  }
  ok(malformedRejected, "a typed callback that erases its result is rejected");
  ok(fetches.length === fetchCountBeforeMalformed, "a malformed callback is rejected before host authority runs");

  app.unmount();
  ok(simulator.pending("interval").length === 0 && simulator.clearCount("interval") === 2, "replacement and unmount each cancel exactly once");
  ok(simulator.pending("timeout").length === 0 && simulator.clearCount("timeout") === 4, "startup replacement, update replacement, explicit cancel, and unmount cancel delayed commands");
  intervalCallback();
  ok(app.getModel() === 1440, "no subscription callback can update after unmount");
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (failures > 0) {
  console.error(`\nGLAMOUR-PROGRAM FAILED (${failures} check(s))`);
  process.exit(1);
}
console.log("\nGLAMOUR-PROGRAM OK");
