# Xz

기계가 쓰고 사람이 믿어야 하는 코드를 위한 범용 프로그래밍 언어.

[![CI](https://github.com/x1zzdev/Xz/actions/workflows/ci.yml/badge.svg)](https://github.com/x1zzdev/Xz/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#라이선스)
[![LLVM 17](https://img.shields.io/badge/backend-LLVM%2017-informational.svg)](docs/13-codegen.md)

AI는 사람이 읽을 수 있는 속도보다 빠르게 프로덕션 코드를 쓴다. 이제 병목은 작성이 아니라 무엇을 믿을지 판단하는 일이다. Xz는 그 판단을 함수 본문이 아니라 시그니처에서 끝내도록 설계했다.

| 리뷰어의 질문 | Xz가 답하는 곳 |
|---|---|
| 무엇을 하는가 | 코드와 대조되는 `@intent` |
| 무엇을 바꿀 수 있는가 | `mut` 바인딩과 `@effects` |
| 무엇을 보장하는가 | `pre` / `post` 계약 |
| 무엇이 실패할 수 있는가 | 단일 오류 채널의 `Result[T, E]` |

코드가 만족하지 못하는 서술은 빌드를 통과하지 못한다. 컴파일러가 증명할 수 없는 서술은 사람이 `@trusted`로 서명해야 한다. 주석이 코드에 대해 거짓말을 할 수 없다.

## 언어 살펴보기

```xz
/// Returns the principal square root of x.
/// @intent  Returns the square root of a non-negative number.
/// @requires x is non-negative
/// @ensures result is ok implies result.value >= 0.0
/// @effects none
func sqrt(x: Float) -> Result[Float, DomainError]
    pre  x >= 0.0
    post result is ok implies result.value >= 0.0
{
    if x < 0.0 {
        err(DomainError("negative input"))
    } else {
        ok(approx_sqrt(x))
    }
}
```

계약은 잊어도 되는 관습이 아니다. 컴파일러가 직접 읽는다.

## 빌드와 실행

최신 Rust 툴체인과 포터블 LLVM 17 설치가 필요하다. 설정 스크립트가 설치를 확인하고 백엔드가 기대하는 환경 변수 두 개를 출력한다.

```sh
git clone https://github.com/x1zzdev/Xz.git
cd Xz/xz-cli
scripts/setup-llvm.sh
cargo run -- run ../examples/hello.xz
```

`contracts.xz`, `concurrency.xz`, `async.xz`, `ffi.xz`, `lists.xz`, `maps.xz`, `sets.xz`, `io.xz`, `time.xz`도 같은 방식으로 실행한다. 각 예제가 보여주는 것은 [examples/README.md](examples/README.md)에 정리돼 있다.

## 현재 상태

지금도 쓸 수 있는 컴파일러다. 프런트엔드(`xz check`)와 LLVM JIT 백엔드(`xz run`)가 동작한다. C ABI 브리지, 공유 라이브러리 출력(`xz build --shared`), Python ctypes 바인딩 생성(`xz bind --lang python`)도 들어가 있다. 타입 지정 채널, 결정적 태스크 스케줄러, `async`/`await`는 JIT 경로에서 실행된다. 언어 서버(`xz lsp`), 포매터(`xz fmt`), 패키지 명령(`xz pkg gen`, `xz pkg add`)도 사용할 수 있다.

Phase 1부터 4까지는 끝났고 나머지는 부분 구현 상태다. 각 Phase의 실제 상태는 금방 낡는 표가 아니라 [docs/08-roadmap.md](docs/08-roadmap.md)에서 관리한다.

## 문서

명세는 `docs/`에 있다. 철학부터 읽고 관심 있는 부분으로 넘어가면 된다.

| 문서 | 내용 |
|---|---|
| [01-philosophy.md](docs/01-philosophy.md) | 설계 철학과 리뷰어의 질문 |
| [02-syntax.md](docs/02-syntax.md) | 문법과 프로그램의 형태 |
| [03-type-system.md](docs/03-type-system.md) | 타입과 계약 지점의 명시성 |
| [04-memory-model.md](docs/04-memory-model.md) | 값 의미론과 명시적 변경 |
| [05-concurrency.md](docs/05-concurrency.md) | 구조적 동시성과 타입 지정 채널 |
| [06-error-handling.md](docs/06-error-handling.md) | `Result` 타입과 단일 오류 채널 |
| [07-compiler.md](docs/07-compiler.md) | 진단, 포매터, 언어 서버 |
| [08-roadmap.md](docs/08-roadmap.md) | 완료된 것과 다음 것 |
| [09-intent-verification.md](docs/09-intent-verification.md) | 선언된 동작을 검증하는 방법 |
| [10-ffi-interop.md](docs/10-ffi-interop.md) | C ABI 브리지와 Python 바인딩 |
| [11-grammar.md](docs/11-grammar.md) | 권위 있는 문법 |
| [12-stdlib.md](docs/12-stdlib.md) | 표준 라이브러리 표면 |
| [13-codegen.md](docs/13-codegen.md) | LLVM 백엔드와 런타임 |
| [14-codegen-notes.md](docs/14-codegen-notes.md) | 백엔드에서 부딪힌 문제와 그 결정 |
| [15-ecosystem.md](docs/15-ecosystem.md) | Xz 코어 위에 올라가는 호스트 통합 |

## 생태계

Xz는 언어와 컴파일러다. 프레임워크는 C ABI와 `.xzint` 인터페이스 형식을 통해 Xz에 연결되므로, 언어 자체를 바꾸지 않고도 호스트 통합을 만들 수 있다.

| 프로젝트 | 호스트 | 역할 |
|---|---|---|
| **Xz** (이 저장소) | 없음 | 언어, 명세, 컴파일러, LLVM 백엔드, C ABI, `.xzint` |
| [next.xz](https://github.com/x1zzdev/next-xz) | Next.js / TypeScript | 바인딩 생성, Bun FFI·Wasm 로더, 에이전트 루프, `/___audit` 오버레이 |
| [rails.xz](https://github.com/imrubydev/rails-xz) | Ruby on Rails | Ruby FFI 브리지, ActiveJob 에이전트 루프, `/xz_audit` 엔진 |

두 툴킷은 같은 경로를 따른다. 에이전트가 계약이 붙은 `.xz` 모듈을 쓰고, `xz check-json`이 수정 루프를 돌리고, `xz build --shared`가 라이브러리를 만들고, 사람이 호스트의 감사 화면에서 계약을 승인한다. 프레임워크가 바뀌어도 코어는 그대로다. 이게 핵심이다. 자세한 내용은 [docs/15-ecosystem.md](docs/15-ecosystem.md)에 있다.

## 기여

기여는 환영한다. 절차는 최대한 평범하게 유지한다. 빌드, 테스트 루프, 그리고 중요한 두 가지 원칙은 [CONTRIBUTING_kr.md](CONTRIBUTING_kr.md)에 있다. 구현보다 명세가 먼저이고, 논리적 변경 하나당 커밋 하나다. 참여자 모두 [행동 강령](CODE_OF_CONDUCT_kr.md)에 동의한다.

## 지원과 보안

질문과 아이디어는 [GitHub Discussions](https://github.com/x1zzdev/Xz/discussions) 또는 이슈 트래커로 보낸다. 취약점은 공개 이슈가 아니라 [SECURITY_kr.md](SECURITY_kr.md)에 적힌 비공개 절차로 제보한다.

## 라이선스

다음 중 하나를 고른다.

- Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

별도로 명시하지 않는 한, Apache-2.0 라이선스에 정의된 대로 이 프로젝트에 제출하는 모든 기여는 위와 동일한 듀얼 라이선스가 적용되며 추가 조건은 없다.

Xz는 프로그래밍 언어이며 `xz` 압축 유틸리티와는 관련이 없다.
