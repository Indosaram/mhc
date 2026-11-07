# 최근 7일 머지 코드 리뷰 (2026-10-29 ~ 2026-11-05)

- 대상: `origin/main` `aefa5c997` (2026-11-06 00:05 KST), 기준점 `70ef47743` (7일 전 main)
- 규모: 커밋 1,684개, 파일 3,444개, +504,383 / -44,010줄. 테스트가 아닌 Rust 추가분만 약 32만 줄
- 주요 머지: omo 5.1.52 upstream sync, `ulw/overhaul`(t5~t28), `ulw/followups`, `fix/pause-resume-deadlock`, `fix/session-memory-2b`, `fix/footer-materialize-memory`, `fix/pty-fork-alloc`, `fu/*` 후속 수정
- 행 번호는 모두 `aefa5c997` 기준

## 리뷰 방법과 한계

50만 줄을 한 줄씩 읽는 것은 불가능해서 두 단계로 나눴다.

1. 7일 diff의 **추가된 줄 전체**를 위험 패턴으로 기계 스캔했다. 대상 패턴은 sleep, `let _ =`, `.ok();`, unwrap/expect, unsafe와 SAFETY 주석, std Mutex, unbounded 채널, `async fn` 안의 동기 IO, `Command::new`, TODO, `#[allow]`이다. 스캔과 함께 파일 크기, 수정 빈도(churn), 의존성 변경, rustfmt 상태도 측정했다.
2. 스캔에 걸린 지점과 churn 상위 파일, 가장 최근 fix 커밋은 직접 읽고 판정했다. 오탐은 아래 목록에서 뺐다. 예를 들어 `start_paused` 가상 시간 테스트의 sleep, 수신자가 사라진 경우를 무시하는 `let _ = tx.send(..)`, 전용 OS 스레드 안의 sleep은 문제가 아니다.

빌드, clippy, 테스트는 실행하지 않았다(읽기 전용 리뷰). 따라서 아래 항목은 정적 판독 결과다. 버그 항목은 수정할 때 실패하는 테스트로 먼저 재현해야 한다.

---

## 1. 버그 / 정확성

### [High] Stop propagation 하나가 panic하면 Resume이 영영 호출되지 않는다
`crates/omo/components/maho-omo-task/src/event_bridge.rs:148-158`

```rust
let propagations = std::mem::take(&mut *pending.lock()...);
for propagation in propagations {
    if propagation.join().is_err() {
        return Err(ExtensionFailure::new("run Stop propagation panicked"));
    }
}
match custody.resume_current() { ... }
```

- `pending`은 `take`로 이미 비운 상태다. 그래서 첫 panic에서 `return`하면 남은 propagation 스레드는 join되지 않은 채 버려진다.
- 더 큰 문제는 `resume_current()`가 아예 호출되지 않는다는 점이다. 그러면 run은 Paused 상태로 남는다. 이번 주 `fix/pause-resume-deadlock`이 막으려던 "일시정지에서 돌아오지 못함" 증상이 다른 경로로 재발할 수 있다.
- 개선:
  ```rust
  let panicked = propagations.into_iter().filter(|p| p.join().is_err()).count();
  let outcome = custody.resume_current();
  // outcome 진단은 기존대로 출력
  if panicked > 0 {
      return Err(ExtensionFailure::new(format!("{panicked} run Stop propagation(s) panicked")));
  }
  ```
  모든 propagation을 join하고, Resume은 **항상** 실행한 다음 에러를 보고하도록 순서를 바꾼다. 회귀 테스트로 "panic하는 propagation 1개 + 정상 1개에서도 Resume이 호출된다"를 고정한다.

### [Med] Resume 부분 실패가 진단 로그에만 남고 run은 복구되지 않는다
`event_bridge.rs:158-176` (`a11d86490`, `fa8bdb5bc`)

- 11-05 두 커밋으로 `Failed`와 `Resumed { failures > 0 | !installed }`를 `diagnostic`으로 남기게 됐다. 그러나 동작은 그대로라서 실패한 참가자는 계속 일시정지 상태다. 사용자 입장에서는 "멈춘 채로 아무 일도 안 일어남"과 구분이 안 된다(진단 로그는 숨겨진 파일이다).
- 개선: 둘 중 하나로 상태를 명시적으로 만든다. (a) 실패한 참가자만 대상으로 bounded 재시도를 한 번 한다. (b) 해당 task를 `Error`/`Interrupted`로 전환하고 notification을 낸다. 어느 쪽이든 "paused인데 아무도 깨우지 않는" 상태가 남지 않게 한다.

### [Med] async OAuth 경로에서 동기 파일 lock을 잡고 `thread::sleep` 재시도
`crates/builtins/maho-ext-mcp/src/auth/token_store.rs:144-185`, 호출부 `auth/oauth.rs:428, 464, 569-570, 682`

- `with_lock`은 `try_lock`이 실패하면 `std::thread::sleep`(20ms에서 시작해 최대 200ms)으로 최대 50회 재시도한다. 최악의 경우 수 초 동안 블로킹된다.
- `async fn begin_authorization_inner`와 `client_credentials_grant`는 `bind_issuer`, `save_client_information`, `invalidate_credentials`를 거쳐 이 함수를 **tokio 워커에서 그대로** 부른다. 여러 MCP 서버가 동시에 인증하거나 다른 프로세스가 lock을 쥐고 있으면 런타임 워커가 묶인다. current_thread 런타임이라면 전체가 멈춘다.
- 같은 모듈의 `oauth_refresh.rs:81, 152`는 이미 `spawn_blocking`으로 감싸고 있어 일관성도 없다.
- 개선: `McpOAuthProvider`의 저장 메서드에 `*_async` 버전을 두고 내부를 `spawn_blocking`으로 감싸 async 호출부가 그것만 쓰게 한다. 또는 `with_lock` 자체를 async(`tokio::time::sleep`) 버전으로 분리한다.

### [Low] 실패해도 조용히 넘어가는 파일 IO
- `crates/builtins/maho-ext-mcp/src/catalog_cache_adoption.rs:77`: 마이그레이션한 캐시를 `let _ = write_mcp_cached_server(..)`로 쓴다. 쓰기가 계속 실패하면 매 시작마다 같은 레거시 마이그레이션이 반복되는데, 그 원인을 알 수 없다. 최소한 `diagnostic` 한 줄은 남겨야 한다.
- `crates/builtins/maho-ext-anthropic-subscription/src/config_dir_credentials.rs:46`: 레거시 계정 디렉터리의 `.bak` rename 실패를 무시한다. 실패하면 legacy 디렉터리가 남아 매번 같은 분기를 탄다. 자격증명 디렉터리라서 실패 사유를 남기는 편이 좋다.

---

## 2. unsafe / 프로젝트 규칙

### [Med] `projfs.rs`가 모듈 전체에서 `undocumented_unsafe_blocks`를 꺼 버렸다
`crates/maho-isolation/src/iso/projfs.rs:120-125` (`8157d8b94`, pi-iso 포팅)

- AGENTS.md 규칙은 "unsafe가 필요한 크레이트는 `deny`로 opt-in하고, **로컬에서** SAFETY 주석과 함께 허용"이다. 그런데 이 파일은 `mod imp` 전체에 `#[allow(clippy::undocumented_unsafe_blocks)]`를 걸었고, 그 결과 `unsafe` 46곳 중 SAFETY 주석은 4개뿐이다.
- 특히 `unsafe impl Send for ProjfsApi {}` / `unsafe impl Sync`(229-230)는 raw `HMODULE`과 함수 포인터를 스레드 간에 공유하는 근거가 전혀 적혀 있지 않다. `GetProcAddress` 결과의 `mem::transmute`(load_symbol 매크로)도 마찬가지로 근거가 없다.
- 개선: 모듈 단위 allow를 지운다. `LoadLibraryW`, `GetProcAddress`, `transmute` 시그니처 계약, `Send/Sync` 근거는 각각 SAFETY 주석으로 적는다. 반복되는 FFI 호출 패턴은 SAFETY를 한 번만 쓰는 얇은 안전 래퍼 함수로 묶는다.

### [Low] 포맷 미적용 코드가 대량 유입됐고 CI에 fmt 게이트가 없다
- 이번 주 추가/수정된 `.rs` 2,470개 중 **901개**가 `rustfmt --edition 2024 --check`를 통과하지 못한다. 분포는 omo/components 113, senpi-task 87, maho-cli 82, maho-core 59, ext-mcp 50 순이다.
- 일부는 여러 문장을 한 줄에 붙여 쓴 형태다(`maho-ext-terminal/src/durable_file.rs:110`, `tools/foreground_detach.rs:43,54`). 이런 코드는 리뷰와 diff 가독성을 크게 떨어뜨린다.
- `.github/workflows/ci.yml`에는 `cargo fmt --check` 단계가 없다.
- 개선: 별도 커밋으로 `cargo fmt --all` 한 번을 적용하고 `.git-blame-ignore-revs`에 등록한다. 그 뒤 CI clippy job 앞에 `cargo fmt --all --check`를 추가한다. 워커가 만든 패치가 포맷 없이 머지되는 경로를 이것으로 막는다.

---

## 3. 구조 / 유지보수성

### [High] `agent_session.rs` 39,894줄, 이번 주에만 71개 커밋
`crates/maho-core/src/agent_session.rs`

- 7일간 34,102줄에서 39,894줄로 늘었다(+5,792). 22,466번째 줄부터 약 17,400줄이 인라인 `mod tests`다. 이를 빼도 소스가 약 22,000줄로, upstream `agent-session.ts`(9,652줄)의 2.3배다.
- 이번 주 수정 커밋 71개 중 26개가 fix다. 한 파일에 수정이 몰려 병렬 레인끼리 머지 충돌이 나고, IDE와 rust-analyzer 응답도 느려진다.
- 개선(file-for-file 규칙을 지키는 범위 안에서):
  1. 인라인 테스트를 `#[cfg(test)] #[path = "agent_session_tests.rs"] mod tests;`로 분리한다. 모듈 대응 관계는 바뀌지 않는다.
  2. upstream에 없는 Rust 전용 보조 로직(이미 `#[path = "monitor_invocation.rs"]` 전례가 있다)은 같은 방식의 하위 모듈로 뺀다.
  3. 파일 크기 상한(예: 소스 8k줄)을 parity 감사 도구의 경고로 추가한다.

### [Med] `multi_session_host.rs`가 한 주 만에 2배가 됐고 fix 비율이 높다
`crates/maho-rpc/src/multi_session_host.rs` 3,396줄에서 7,151줄로, 53개 커밋 중 27개가 fix다.

- 같은 기간 `maho-rpc/src`는 268개 파일로 늘었고 `lib.rs`는 56번 수정됐다. host lifecycle 관련 동작(`host_ensure*`, `host_lifecycle*`, `host_readiness`, `host_successor`, `host_supervisor_poll`)은 각각 자체 폴링 루프와 데드라인 상수를 갖고 있다.
- 개선: 공통 "deadline 안에서 조건이 될 때까지 대기" 헬퍼 하나를 만든다(입력은 interval, deadline, 조건 클로저). 현재 `host_ensure.rs:686`, `host_ensure_stop.rs` `wait_for_gone`, `host_readiness.rs:85`, `host_successor.rs:91`, `host_legacy.rs:202`가 거의 같은 형태를 각자 구현하고 있다. 데드라인 상수도 한 모듈에 모으면 플랫폼별 튜닝(Windows named pipe 등)을 한곳에서 할 수 있다.

### [Low] senpi-task: 추적 child마다 OS 스레드 1개 + 최대 2초 블로킹 대기
`crates/omo/senpi-task/src/manager/outcome.rs:95, 119`, `provisional_exit.rs:43-55`

- `track_outcome`, `track_exact_outcome`는 child마다 `std::thread::spawn`을 하고, 그 스레드에서 `wait_exit_confirmation`(기본값 `thread::sleep`)으로 최대 `UNEXPECTED_EXIT_CONFIRMATION_MS = 2_000`ms를 기다린다. 런타임은 막지 않으므로 지금은 정확하다. 다만 동시 child 수만큼 스레드가 생기고, 대기는 sleep 루프로만 끝난다.
- 개선(급하지 않음): 종료 확인은 store 변경 이벤트를 구독하고 `deadline`까지 기다리는 방식으로 바꾼다. 그러면 확인이 일찍 끝나도 남은 시간을 다 기다리지 않는다. 스레드 대신 블로킹 풀(`spawn_blocking`)이나 하나의 감시 스레드로 모으는 것도 검토한다.

### [Low] async 함수 안의 동기 파일 IO
- 대부분 `metadata`나 작은 파일이라 실제 영향은 작다. 다만 아래는 런타임 위에서 무거운 작업을 할 수 있다.
  - `maho-omo-comment-checker/src/downloader.rs:78`, `maho-ext-pi-ast-grep/src/binary_downloader.rs:39`, `maho-cli/src/utils/tools_manager.rs:119`: 아카이브 해제와 디렉터리 순회를 async 함수 안에서 동기로 수행한다.
  - `maho-core/src/package_manager.rs:850`: `run` 안의 디렉터리 생성과 복사.
- 개선: 다운로드 후 해제 단계는 `spawn_blocking`으로 감싼다.

---

## 4. 잘 된 점 (유지할 것)

- 테스트의 시간 처리가 대체로 규칙을 지킨다. 스캔에서 걸린 테스트 sleep은 모두 `#[tokio::test(start_paused = true)]` 가상 시간을 쓰거나(codemode idle parking, cache-keepalive, rpc lifecycle ticker 등), 시간 자체를 검증하는 경우(`maho-ai/tests/openai_responses.rs:567`, 주석으로 명시)였다.
- `zip`을 RUSTSEC-2026-0429 이후 버전으로 올렸고, `tokio` 등 버전 고정을 workspace 의존성으로 모았다. `cargo-deny` job과 MIT 프로필 라이선스 게이트가 CI에 있다.
- PTY fork-exec 사이 할당 금지(`[patch.crates-io]` portable-pty)나 footer의 세션 전체 materialize 제거처럼, 원인을 고치는 수정이 들어왔다.

---

## 5. 우선순위 제안

| 순서 | 항목 | 이유 |
|---|---|---|
| 1 | event_bridge Resume 누락 (1-1) | run이 영구 정지할 수 있는 정확성 버그이고 수정 범위가 작다 |
| 2 | OAuth 동기 lock (1-3) | 동시 인증에서 런타임 정지 가능성이 있고 수정이 기계적이다 |
| 3 | rustfmt 일괄 적용 + CI fmt 게이트 (2-2) | 이후 리뷰 비용 전체를 낮추고 1회성 작업이다 |
| 4 | Resume 부분 실패 상태 전환 (1-2) | 사용자에게 보이는 "멈춤"을 없앤다 |
| 5 | projfs SAFETY 정리 (2-1) | 프로젝트 unsafe 규칙 준수 |
| 6 | agent_session 테스트 분리, host 대기 헬퍼 (3-1, 3-2) | 충돌과 회귀 빈도를 줄이는 구조 개선 |
| 7 | 나머지 Low | 여유 있을 때 |

---

## 6. 적용 후 검증에서 발견한 것 (2026-11-07)

수정 브랜치 `origin/fix/weekly-review-20261105`를 현재 main(`1c6ba2ee9`)에 병합하고 검증하는 과정에서, 위 항목들의 수정 자체에 결함 2건이 확인됐다.

### [Med] 크기 상한 검사가 정작 대상 파일을 보지 못했다
`tools/parity-audit.mjs` `nonTestLineCount` — 3-1의 "파일 크기 상한" 구현.

첫 번째 최상위 `#[cfg(test)]` 줄에서 계산을 멈추는 방식이었다. `agent_session.rs`는 추출된 테스트 모듈보다 **앞쪽**에 작은 `#[cfg(test)]` 헬퍼 5개(84줄)를 두고 있어서, 22,606줄짜리 파일을 **3,749줄**로 보고했다. 상한(8,000)을 넘지 않는다고 판정해 경고조차 나오지 않았다. 즉 상한이 존재하는 유일한 이유인 파일을 상한이 놓치고 있었다. 전체 스캔에서 이 방식이 놓친 파일은 정확히 `agent_session.rs` 하나였다.

수정: `#[cfg(test)]` 항목을 중괄호 균형으로 건너뛰고 그 뒤 소스를 계속 세도록 바꿨다. 문자열/주석 안의 중괄호는 세지 않는다. 결과는 `agent_session.rs` 22,522줄(실제 테스트 영역 84줄만 제외)로 정확해졌다. self-test에 "앞쪽 인라인 `#[cfg(test)]` 헬퍼 뒤의 거대 소스도 보고된다" 회귀를 추가했고 28/28 통과한다.

참고: 이 상한은 경고만 출력하고 exit code를 바꾸지 않으며, parity-audit은 CI에서 실행되지 않는다. 그래서 이 결함은 어떤 게이트도 실패시키지 않았고, 조용히 아무것도 잡지 못하는 상태였다.

### [Low] clippy가 수정 브랜치 코드에서 실패했다
`crates/omo/components/maho-omo-task/src/dag_retention_sweep.rs:29` — 중복 클로저(`std::thread::spawn(move || sweep())`). main에는 이 구조체가 없어 브랜치 유래임을 확인하고 `std::thread::spawn(sweep)`으로 고쳤다. 브랜치는 머지된 적도 CI를 통과한 적도 없어서 드러나지 않았던 것이다.

