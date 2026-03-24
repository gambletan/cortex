# Cortex

[![GitHub stars](https://img.shields.io/github/stars/gambletan/cortex?style=social)](https://github.com/gambletan/cortex/stargazers)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

[English](README.md) | [中文](README_CN.md) | [日本語](README_JA.md)

### 프라이버시 우선. 완전 무료. 로컬 실행. — 개인 AI 에이전트를 위한 기억 엔진.

**AI의 기억은 당신의 기기에만 존재합니다 — 외부로 전송되지 않고, 비용도 없으며, 감시도 없습니다.** 순수 Rust. 3.8MB. 서드파티 서버 없음. 텔레메트리 없음. 비용 없음. 자신의 클라우드 스토리지를 통해 동기화합니다.

> **철학:** 당신의 기억은 당신의 것입니다 — 클라우드 제공업체의 학습 데이터가 아니고, 스타트업의 수익화 자산도 아니며, 누군가의 감시 대상도 아닙니다. Cortex는 100% 당신의 하드웨어에서 실행되고, 모든 데이터를 당신의 데이터베이스에 저장하며, 오직 당신의 클라우드 스토리지(iCloud, Google Drive, OneDrive, Dropbox)를 통해서만 동기화합니다. 어떤 중간자도 당신의 데이터를 볼 수 없습니다. API 키가 필요 없고, 계정을 만들 필요도 없습니다. AI 에이전트에 연결하기만 하면 — 프라이빗하게, 영구적으로, 서브밀리초 속도로 기억합니다.

LLM은 매 세션을 빈 상태로 시작합니다. 당신의 어시스턴트는 당신의 이름도, 선호도도, 어제 나눈 대화도, 지난주에 내린 결정도 잊어버립니다. 현재의 "메모리" 솔루션들은 평문 텍스트 파일이거나, 키워드 grep이거나, 200-500ms의 지연을 추가하고 비용을 청구하며 개인 데이터를 타인의 서버로 전송하는 클라우드 API입니다.

Cortex가 이 문제를 해결합니다. AI에게 구조화되고 쿼리 가능하며 자기 진화하는 장기 기억을 제공합니다. 세션, 채널, 컨텍스트를 넘나들며 지속됩니다 — 자기 수정하는 베이지안 신뢰 체계, 플랫폼을 넘나들며 신원을 통합하는 사람 그래프, 모든 연산에서 서브밀리초 성능. 전부 로컬에서 실행되고, 전부 당신의 것입니다.

### Cortex vs Mem0 vs OpenAI Memory

| | Cortex | Mem0 | OpenAI Memory |
|---|---|---|---|
| **프라이버시** | 100% 로컬, 클라우드 없음 | 클라우드 API (데이터가 그들의 서버에) | OpenAI 서버 |
| **지연시간** | **156µs** 저장, **568µs** 검색 | ~200-500ms | ~300-800ms |
| **비용** | 영구 무료 | $99+/월 (Pro) | ChatGPT Plus ($20/월) |
| **기억 계층** | 4단계 (작업/에피소드/시맨틱/절차) | 1단계 (평면) | 1단계 (평면) |
| **베이지안 신뢰** | 증거 기반 자기 수정 | 없음 | 없음 |
| **사람 그래프** | 채널 간 신원 통합 | 유료 플랜만 | 없음 |
| **대화 압축** | 자동 세션 요약 | 없음 | 없음 |
| **관계 추론** | 패턴 기반 (영어 + 중국어) | 없음 | 없음 |
| **시간 검색** | 의도 인식 ("최근" / "처음으로") | 없음 | 없음 |
| **모순 감지** | 자동 감지 + 신뢰도 점수 | 없음 | 없음 |
| **통합 엔진** | 에피소드 → 시맨틱 자동 승격 | 없음 | 없음 |
| **컨텍스트 주입** | 토큰 예산 기반 LLM 출력 | 수동 | 자동이나 불투명 |
| **가져오기/내보내기** | 완전한 JSON 백업 & 복원 | API만 | 내보내기 없음 |
| **자체 호스팅** | 네이티브 바이너리, Docker, MCP | 클라우드만 | 클라우드만 |
| **바이너리 크기** | 3.8 MB | npm 패키지 | N/A |
| **의존성** | 런타임 의존성 0개 | Node.js + 클라우드 | N/A |
| **오픈소스** | MIT | 부분 공개 | 아니오 |
| **암호화** | AES-256-GCM 암호화 동기화 (선택사항) | 없음 | 없음 |
| **프라이버시 레벨** | Private (기본, 동기화 안 함) / Shared / Public | 없음 | 없음 |
| **텔레메트리 없음** | 분석 없음, 외부 전송 없음, 검증 가능 | 불명 | 아니오 |
| **비용** | 영구 무료, 무제한 | $99+/월 (Pro) | $20/월 (Plus) |
| **중국어 NLP** | 네이티브 지원 (추론, 검색, 관계) | 없음 | 제한적 |
| **네임스페이스 격리** | 사용자/컨텍스트별 기억 분리 | 없음 | 없음 |
| **플러그인 시스템** | 저장/검색/통합을 위한 컴파일 타임 훅 | 없음 | 없음 |
| **MCP 도구** | Claude/LLM 통합용 25개 도구 | 서드파티 | N/A |

### 성능 벤치마크

| 연산 | Cortex | Mem0 (클라우드) | 파일 기반 |
|-----------|--------|-------------|------------|
| 저장 | **156µs** | ~200ms | ~1ms |
| 검색 (top-10) | **568µs** | ~300ms | ~10ms |
| 컨텍스트 생성 | **621µs** | ~500ms | 수동 |
| 신뢰 업데이트 | **66µs** | N/A | N/A |
| 사람 그래프 | **51µs** | 유료 플랜 | N/A |
| 구조화된 사실 | **45µs** | N/A | N/A |
| 1K 기억 검색 | **1.6ms** | ~500ms | ~50ms |

Mem0 클라우드보다 **528배 빠릅니다**. Mem0와 OpenAI Memory가 제공하지 않는 기능들을 포함합니다.

> **참고:** 벤치마크는 모든 저장 연산 시 능동적 추론(사실, 선호도, 관계 자동 추출)을 포함합니다. 추론 없는 순수 저장은 ~15µs입니다. M 시리즈 Mac에서 `cargo bench`로 측정한 수치입니다.

### LoCoMo 벤치마크 ([ACL 2024](https://snap-research.github.io/locomo/))

학술 수준의 장기 대화 기억 평가 — 10개 대화, 4개 카테고리, 1,540개 QA 쌍.

| 시스템 | 단일 추론 | 다단계 추론 | 개방형 | 시간 | 전체 |
|--------|-----------|-----------|-------------|----------|---------|
| Backboard | 89.4% | 75.0% | 91.2% | 91.9% | 90.0% |
| MemMachine v0.2 | — | — | — | — | 84.9% |
| **Cortex v1.7** | **72.5%** | **59.5%** | **88.8%** | **74.1%** | **73.7%** |
| Mem0-Graph | 65.7% | 47.2% | 75.7% | 58.1% | 68.4% |
| Mem0 | 67.1% | 51.2% | 72.9% | 55.5% | 66.9% |
| OpenAI Memory | — | — | — | — | 52.9% |

**주요 결과:**
- **개방형 88.8%** — Mem0 (72.9%) 대비 +15.9% 우위
- **시간 추론 74.1%** — Mem0 (55.5%) 대비 +18.6% 우위
- **단일 추론 72.5%** — Mem0 (67.1%) 대비 +5.4% 우위
- **다단계 추론 59.5%** — Mem0 (51.2%) 대비 +8.3% 우위
- **전체 73.7%** — Mem0 (66.9%) 대비 +6.8%, OpenAI Memory (52.9%) 대비 +20.8% 앞섬

Cortex는 4개 카테고리 전체에서 Mem0를 능가합니다 — 100% 로컬 실행, 종단 간 암호화, 비용 $0으로.

> **설정:** Claude Sonnet 4 (QA + 평가), nomic-embed-text (Ollama를 통한 임베딩), top-30 검색. 완전 재현 가능: `python3 bench/locomo_bench.py`

## 아키텍처

Cortex는 인간의 인지에서 영감을 받은 4단계 기억 모델을 구현합니다:

```
                    +---------------------+
                    |   Working Memory    |  현재 세션 컨텍스트
                    +---------------------+
                              |
                    +---------------------+
                    |   Episodic Memory   |  원시 경험: 대화, 이벤트, 관찰
                    +---------------------+
                              |  통합 (감쇠, 승격, 패턴 추출)
                    +---------------------+
                    |   Semantic Memory   |  정제된 사실, 선호도, 관계
                    +---------------------+
                              |
                    +---------------------+
                    | Procedural Memory   |  학습된 루틴, 사용자별 워크플로우
                    +---------------------+
```

**Working(작업)**은 현재 세션의 스크래치 패드를 보유합니다. **Episodic(에피소드)**은 타임스탬프와 소스 메타데이터가 포함된 원시 경험을 저장합니다. **통합 엔진**은 주기적으로 반복되는 패턴을 **Semantic(시맨틱)** 사실로 승격시키고 오래된 에피소드를 감쇠시킵니다. **Procedural(절차)**은 학습된 워크플로우와 루틴을 포착합니다.

## 핵심 구성요소

### 사람 그래프
채널 간 신원 통합. Telegram으로 메시지를 보내고, 이메일을 보내고, 캘린더 이벤트에 등장하는 동일한 사람이 하나의 신원 노드로 통합됩니다. 상호작용, 관계 강도, 소통 패턴이 사람별로 추적됩니다.

### 베이지안 신뢰 시스템
자기 수정하는 세계 이해. 신뢰는 증거로부터 형성되고, 새로운 관찰이 있을 때마다 업데이트되며, 반박될 수 있습니다. 신뢰도 점수는 최신성 편향이 아닌 실제 확실성을 반영합니다.

```rust
cortex.observe_belief("user_prefers_morning_meetings", true, 0.8)?;
cortex.observe_belief("user_prefers_morning_meetings", false, 0.6)?;
// 베이지안 업데이트를 통해 신뢰도가 자동으로 조정됩니다
```

### 통합 엔진
에피소드에서 시맨틱으로의 승격, 오래된 기억의 감쇠, 패턴 추출. 기억 저장소를 효율적이고 쿼리 가능한 상태로 유지하는 백그라운드 사이클로 실행됩니다. 승격, 감쇠, 병합된 항목에 대한 리포트를 반환합니다.

### 다중 신호 검색
쿼리는 관련성 순위를 위해 다섯 가지 신호를 결합합니다:
- **유사도** -- 쿼리 임베딩에 대한 벡터 코사인 거리
- **시간** -- 설정 가능한 감쇠가 적용된 최신성 가중치
- **중요도** -- 접근 패턴과 명시적 힌트 기반의 중요성 점수
- **소셜** -- 특정 사람과 관련된 기억에 대한 가중치 증가
- **채널** -- 소스 채널에 따른 필터링 또는 가중치 조정

### 컨텍스트 주입 프로토콜
기억 상태로부터 LLM 즉시 사용 가능한 컨텍스트 문자열을 생성합니다. 토큰 예산, 선택적 채널/사람 필터를 전달하면 LLM이 직접 소비할 수 있는 구조화된 텍스트 블록을 반환합니다.

### 스토리지
지속성을 위한 SQLite, 빠른 유사도 검색을 위한 인메모리 벡터 인덱스. 단일 파일 데이터베이스, 외부 서비스 불필요. 엣지 배포를 위해 설계됨 — 노트북, 라즈베리 파이, 서버 모두에서 실행됩니다.

### 클라우드 동기화

자신의 클라우드 스토리지를 통해 기기 간 기억을 동기화합니다 — 서드파티 서버를 거치지 않습니다.

```
기기 A (Mac)              내 클라우드 스토리지              기기 B (iPhone)
┌──────────┐         ┌──────────────────────┐         ┌──────────┐
│ SQLite DB │ ──쓰기→  │ iCloud / GDrive /    │  ←읽기── │ SQLite DB│
│ (로컬)    │         │ OneDrive / Dropbox   │         │ (로컬)   │
│           │ ←읽기──  │                      │  ──쓰기→ │          │
└──────────┘         └──────────────────────┘         └──────────┘
```

- **변경 로그 기반**: 각 기기는 자체 서브폴더에 추가 전용 작업 로그를 씁니다
- **충돌 없음**: 기기들은 동일한 파일에 쓰지 않습니다. 하이브리드 논리 시계(HLC)를 사용한 Last-Writer-Wins 병합
- **암호화**: AES-256-GCM 암호화 (선택사항). 클라우드 계정이 침해되더라도 기억은 안전하게 유지됩니다
- **프라이버시 인식**: Private 기억(기본값)은 절대 기기를 떠나지 않습니다. Shared/Public 기억만 동기화됩니다

지원 제공자: **iCloud Drive**, **Google Drive**, **OneDrive**, **Dropbox** (자동 감지).

```rust
use cortex_core::sync::SyncConfig;

// 암호화와 함께 동기화 활성화
let config = SyncConfig::new(sync_dir, device_id, device_name)
    .with_encryption("my-strong-passphrase");
cortex.enable_sync(config)?;

// 다른 기기의 변경사항 가져오기
let applied = cortex.sync_pull()?;
println!("Applied {} remote changes", applied);
```

### 보안 및 프라이버시

| 기능 | 세부 사항 |
|---------|--------|
| **암호화** | Argon2id 키 유도를 사용하는 AES-256-GCM (라인당 무작위 nonce) |
| **프라이버시 레벨** | Private (기본, 동기화 안 함), Shared, Public |
| **메모리 초기화** | 드롭 시 RAM에서 민감한 데이터 삭제 (`zeroize` 크레이트) |
| **텔레메트리 없음** | 분석 없음, 외부 전송 없음. 검증: `grep -r "reqwest\|hyper\|TcpStream" cortex-core/src/` |
| **계정 없음** | API 키 없음, 등록 없음, 클라우드 의존성 없음 |

전체 위협 모델은 [SECURITY.md](SECURITY.md)를 참조하세요.

## 사전 요구사항

[Rust 툴체인](https://rustup.rs/)을 설치합니다 (`cargo` 제공):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

설치 후 터미널을 재시작하거나 다음을 실행합니다:
```bash
source "$HOME/.cargo/env"
```

설치 확인:
```bash
cargo --version
```

## 실제 사용 예: 진짜로 기억하는 AI 어시스턴트

일주일간의 실제 대화에서 AI 어시스턴트의 모습을 상상해 보세요:

```
# 1일차 — Telegram에서 대화
나: "Kakao의 지수와 미팅이 잘 됐어. 우리 API에 관심 있대."

  Cortex 자동 추출:
  ├── 에피소드 기억 저장됨 (156µs)
  ├── 사실: 지수 → 근무지 → Kakao (신뢰도: 0.85)
  ├── 사실: 지수 → 관심 → 우리 API
  └── 신원 해석: jisu_telegram

# 2일차 — 지수가 이메일을 보냄
발신: jisu@kakao.com
"우리가 논의한 기술 명세서입니다."

  Cortex:
  ├── 신원 해석: jisu@kakao.com → jisu_telegram에 통합
  │   (동일 인물, 다른 채널 — 자동 신원 통합)
  └── 사실: 지수 → 전송 → 기술 명세서

# 3일차 — AI에게 질문
나: "Kakao 쪽 진행 상황이 어때?"

  Cortex 검색 (568µs):
  ├── 지수는 Kakao에서 근무 (시맨틱 사실)
  ├── 미팅 순조롭게 진행, API에 관심 (에피소드, 1일차)
  ├── 기술 명세서 전송 (에피소드, 2일차)
  └── 채널 간 컨텍스트: Telegram + Email이 한 사람으로 통합

  AI가 완전한 컨텍스트로 답변 — "죄송합니다, 기억이 나지 않아요"는 없음

# 5일차 — 새로운 정보 도착
나: "지수가 지난달에 Naver로 이직했대."

  Cortex:
  ├── 모순 감지: 지수 근무지 Kakao vs 지수 근무지 Naver
  ├── 이전 사실 신뢰도 감쇠: Kakao (0.85 → 0.15)
  ├── 새 사실 저장: 지수 → 근무지 → Naver (0.90)
  └── 베이지안 추론을 통해 신뢰 자동 업데이트 — 수동 정리 필요 없음

# 7일차 — 통합 엔진 실행
  Cortex 자동 통합:
  ├── 지수에 관한 에피소드 기억 3개 → 시맨틱 요약으로 승격
  ├── 다른 주제의 오래된 기억 → 감쇠
  └── 패턴 감지: 매주 월요일에 정기 미팅이 있음
```

이 모든 것이 **로컬에서 연산당 1ms 미만**으로 처리됩니다. 클라우드 없음. API 호출 없음. 아무도 당신의 데이터를 볼 수 없습니다.

## 빠른 시작

```rust
use cortex_core::Cortex;

// 기억 데이터베이스 열기 (또는 생성)
let cortex = Cortex::open("memory.db")?;

// Telegram 대화에서 기억 저장
let embedding = your_embedding_fn("Q3 로드맵에 대해 Alice와 미팅함");
cortex.ingest(
    "Q3 로드맵에 대해 Alice와 미팅함",
    "telegram",               // 소스 채널
    Some("alice_123"),         // 사용자 ID (신원 해석 트리거)
    Some(0.8),                 // 중요도 힌트
    Some(embedding),           // 벡터 임베딩
)?;

// 시맨틱 사실 직접 추가
cortex.add_fact(
    "Alice", "works_at", "Acme Corp",
    0.95, "telegram", None,
)?;

// 선호도 저장
cortex.add_preference("timezone", "Asia/Seoul", 0.9)?;

// 관련 기억 검색
let results = cortex.retrieve(
    "Alice에 대해 알고 있는 것은?",
    5,                         // top-k
    None,                      // 모든 채널
    None,                      // 모든 사람
    Some(query_embedding),     // 유사도 검색용 벡터
)?;

// LLM 즉시 사용 컨텍스트 생성 (토큰 예산 적용)
let context = cortex.get_context(
    2000,                      // 최대 토큰
    Some("telegram"),          // 채널 필터
    None,                      // 사람 필터 없음
)?;
// LLM에 시스템/사용자 메시지 접두사로 `context` 전달

// 통합 실행 (주기적으로 호출)
let report = cortex.run_consolidation()?;
println!("Promoted: {}, Decayed: {}", report.promoted, report.decayed);
```

## Python 바인딩

[PyO3](https://pyo3.rs)를 통해 곧 제공될 예정입니다. `cortex-python` 크레이트는 전체 API를 네이티브 Python 모듈로 노출합니다:

```python
from cortex import Cortex

cx = Cortex.open("memory.db")
cx.ingest("Bob과 태국 음식점에서 점심 먹음", channel="imessage", user_id="bob")
results = cx.retrieve("Bob이 좋아하는 식당은?", limit=5)
```

## unified-channel-hub와의 통합

Cortex는 [unified-channel-hub](https://github.com/gambletan/unified-channel-hub)의 기억 레이어로 설계되었습니다. 어떤 채널 어댑터에서든 메시지가 유입되면 Cortex가 이를 저장하고 인덱싱한 뒤, 컨텍스트 주입 프로토콜이 각 응답 전에 관련 기억을 LLM에 제공합니다.

```
Telegram ─┐                          ┌─ Context
Discord  ─┤  unified-channel-hub  →  │  Cortex  →  LLM
Email    ─┤  (ingest)                 │  (retrieve + inject)
Calendar ─┘                          └─ Response
```

## MCP 서버 (Claude Code / Claude Desktop)

Cortex는 MCP 서버로 제공됩니다 — MCP 호환 클라이언트라면 어디서든 사용할 수 있습니다.

### 설정

**1. 바이너리 빌드 및 설치:**

```bash
mkdir -p ~/.local/bin ~/.cortex
cargo build --release -p cortex-mcp-server
cp target/release/cortex-mcp-server ~/.local/bin/
```

**2. MCP 서버로 등록:**

Claude Code (CLI):
```bash
# 전역 (모든 프로젝트)
claude mcp add cortex --scope user -- ~/.local/bin/cortex-mcp-server ~/.cortex/memory.db

# 또는 프로젝트별
claude mcp add cortex -- ~/.local/bin/cortex-mcp-server ~/.cortex/memory.db
```

Claude Desktop — `~/Library/Application Support/Claude/claude_desktop_config.json`에 추가:
```json
{
  "mcpServers": {
    "cortex": {
      "command": "/Users/you/.local/bin/cortex-mcp-server",
      "args": ["/Users/you/.cortex/memory.db"]
    }
  }
}
```

**3. 도구를 "확인 없이" 모드로 허용:**

`~/.claude/settings.json` → `permissions.allow`에 추가:
```json
"mcp__cortex__*"
```

> 참고: MCP 도구 권한은 괄호 형식(예: `mcp__cortex__memory_ingest(*)`)을 지원하지 않습니다. 대신 와일드카드 `mcp__cortex__*`를 사용하세요.

**4. 자동 기억 활성화** — `CLAUDE.md`(프로젝트 또는 전역 `~/.claude/CLAUDE.md`)에 추가:

```markdown
# 기억 (Cortex)
Cortex MCP 도구를 통한 영구 기억을 보유하고 있습니다. 자동으로 사용하세요:
- 대화 시작 시: `memory_context`를 호출하여 사용자에 대해 알고 있는 내용 로드
- 사용자가 선호도, 사실, 개인 정보를 공유할 때: `memory_ingest`를 호출하여 저장
- 구조화된 사실을 알게 될 때: `fact_add` 호출 (예: "User works_at Google")
- 선호도를 감지할 때: `preference_set` 호출 (예: editor=neovim)
- 증거가 신뢰를 지지하거나 반박할 때: `belief_observe` 호출
- 새로운 사람과 대화할 때: `person_resolve`를 호출하여 신원 추적
- 주기적으로: `memory_consolidate`를 호출하여 오래된 기억 정리
```

**5. 세션 시작 시 기억 자동 주입** (Claude Code 훅 — 완전 자동):

`~/.claude/hooks/cortex-memory-inject.sh` 생성:
```bash
#!/bin/bash
CORTEX_BIN="${CORTEX_BIN:-$HOME/.local/bin/cortex-mcp-server}"
CORTEX_DB="${CORTEX_DB:-$HOME/.cortex/memory.db}"
[ -x "$CORTEX_BIN" ] || exit 0

printf '%s\n%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"hook","version":"1.0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_context","arguments":{"max_tokens":1500}}}' \
  | "$CORTEX_BIN" "$CORTEX_DB" 2>/dev/null \
  | grep '"id":2' \
  | python3 -c "import sys,json; r=json.load(sys.stdin); print(r['result']['content'][0]['text'])" 2>/dev/null
```

`~/.claude/settings.json`에 추가:
```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/cortex-memory-inject.sh"
          }
        ]
      }
    ]
  }
}
```

이제 모든 새 Claude Code 세션이 자동으로 기억 컨텍스트를 로드합니다 — **수동 작업 없음**. Claude는 작업하면서 학습하고 세션 간에 기억합니다.

### 다중 프로젝트 격리

여러 프로젝트에 걸쳐 작업하시나요? 물리적 기억 격리를 위해 **별도의 데이터베이스**를 사용하세요 — 프로젝트 간 유출 없음, 코드 변경 없음.

```
~/.cortex/
├── global.db          # 사용자 선호도, 사람 그래프, 프로젝트 간 지식
├── my-app.db          # 프로젝트 A 기억
└── my-api.db          # 프로젝트 B 기억
```

**전역 설정** (`~/.claude/settings.json`) — 사용자 수준 지식:
```json
{
  "mcpServers": {
    "cortex-global": {
      "command": "~/.local/bin/cortex-mcp-server",
      "args": ["~/.cortex/global.db"]
    }
  },
  "permissions": { "allow": ["mcp__cortex-global__*", "mcp__cortex-project__*"] }
}
```

**프로젝트별 설정** (`~/.claude/projects/<path>/settings.json`) — 프로젝트 전용:
```json
{
  "mcpServers": {
    "cortex-project": {
      "command": "~/.local/bin/cortex-mcp-server",
      "args": ["~/.cortex/my-app.db"]
    }
  }
}
```

그 다음 프로젝트의 `CLAUDE.md`에 다음 기억 격리 규칙을 추가하세요:

```markdown
## 기억 격리

두 개의 Cortex MCP 서버: `cortex-project` (프로젝트 DB)와 `cortex-global` (전역 DB).

### 쓰기 정책
- 이 저장소의 아키텍처, 코드, 모듈, 테스트, 워크플로우, 설정, 버그, 결정, 용어에 관한 기억은 `cortex-project`에 저장.
- 장기 사용자 선호도, 커뮤니케이션 스타일, 프로젝트 간 습관, 저장소 전반에 걸쳐 유용한 개인 배경만 `cortex-global`에 저장.
- **기본값: 불확실한 경우 `cortex-project`에 저장.**

### 읽기 정책
1. 먼저 `cortex-project` 쿼리.
2. 사용자 수준 선호도에 대해서만 `cortex-global` 쿼리.
3. 충돌 시 프로젝트 기억 우선.

### 유출 방지 규칙
- `cortex-project`에서 `cortex-global`로 자동 복사하지 않기.
- 저장소별 경로, 모듈 이름, 계정 이름을 `cortex-global`에 저장하지 않기.
- 프로젝트 구현 세부사항을 사용자 전역 선호도로 취급하지 않기.

### 업데이트 규칙
- Cortex는 추가 전용입니다. 업데이트 방법: 이전 항목 검색 → 삭제 → 새 항목 저장.
```

이렇게 하면 프로젝트당 두 개의 독립적인 Cortex 인스턴스를 갖게 됩니다 — 공유 사용자 지식을 유지하면서 완전한 격리.

### 27개 도구

| 도구 | 목적 |
|------|---------|
| `memory_ingest` | 기억 저장 (텍스트, 채널, 사람 컨텍스트) |
| `memory_search` | 모든 기억 계층에 걸친 시맨틱 검색 |
| `memory_context` | LLM 즉시 사용 컨텍스트 요약 생성 (토큰 예산) |
| `memory_consolidate` | 감쇠 + 승격 + 정리 사이클 실행 |
| `memory_infer` | 저장 없이 추론 미리보기 |
| `memory_compress` | 이전 대화 세션 압축 |
| `memory_stats` | 기억 통계 조회 (계층별 수, 인덱스 크기) |
| `memory_decay` | 에피소드 기억에 시간 감쇠 실행 |
| `belief_observe` | 증거와 함께 베이지안 신뢰 업데이트 |
| `belief_list` | 신뢰도 임계값 이상의 신뢰 쿼리 |
| `fact_add` | 구조화된 지식 저장 (주어-술어-목적어) |
| `fact_query` | 엔터티별 사실 쿼리 (SQL 인덱스) |
| `preference_set` | 신뢰도와 함께 사용자 선호도 저장 |
| `preference_query` | 키 패턴으로 선호도 쿼리 |
| `person_resolve` | 채널 간 신원 해석 |
| `person_list` | 알려진 모든 사람 목록 조회 |
| `contradiction_check` | 사실 모순 확인 |
| `relationship_extract` | 텍스트에서 관계 추출 |
| `sync_status` | 클라우드 동기화 상태 (제공자, 기기, 대기 중인 작업) |
| `sync_providers` | 사용 가능한 클라우드 스토리지 제공자 감지 |

## OpenClaw 플러그인

OpenClaw 에이전트에 자동 기억 및 자동 회상 기능을 갖춘 영구 기억을 제공합니다.

**설치:**

```bash
# 1. Cortex 바이너리 설치
curl -fsSL https://raw.githubusercontent.com/gambletan/cortex/main/install.sh | bash

# 2. OpenClaw 플러그인 설치
openclaw plugin add @cortex-ai-memory/cortex-memory
```

**설정** (선택사항 — 기본값으로 작동):

```json
{
  "plugins": {
    "@cortex-ai-memory/cortex-memory": {
      "autoCapture": true,
      "autoRecall": true,
      "topK": 10
    }
  }
}
```

**기능:**
- `autoCapture`: 매 턴 후 자동으로 대화 컨텍스트 저장
- `autoRecall`: 매 턴 전에 관련 기억 주입 (에이전트가 "기억"함)
- 7개 도구: memory_search, memory_store, fact_add, belief_observe, person_resolve 등

전체 설정 옵션은 `openclaw-plugin/README.md`를 참조하세요.

## 프로젝트 구조

```
cortex/
├── cortex-core/          # Rust 핵심 라이브러리 (모든 기억 로직)
│   ├── src/
│   │   ├── lib.rs              # Cortex 진입점
│   │   ├── types.rs            # MemObject, MemoryTier 등
│   │   ├── inference.rs        # 능동적 추론 (영어 + 중국어)
│   │   ├── episode.rs          # 에피소드 기억 저장소
│   │   ├── semantic.rs         # 시맨틱 사실 + 선호도
│   │   ├── working.rs          # 작업 기억 (세션 스크래치 패드)
│   │   ├── procedural.rs       # 학습된 루틴
│   │   ├── people.rs           # 사람 그래프 + 신원 해석
│   │   ├── belief.rs           # 베이지안 신뢰 시스템
│   │   ├── consolidation.rs    # 에피소드→시맨틱 승격 + 감쇠
│   │   ├── retrieval.rs        # 다중 신호 검색 엔진
│   │   ├── context.rs          # LLM 컨텍스트 생성
│   │   ├── sync/               # 클라우드 동기화 (oplog, HLC, merge, 암호화)
│   │   └── storage/            # SQLite + 인메모리 벡터 인덱스
│   └── benches/                # 성능 벤치마크
├── cortex-http/          # HTTP REST API (axum, 로컬 전용)
├── cortex-mcp-server/    # MCP 서버 바이너리 (3.8MB)
├── cortex-python/        # Python 바인딩 (PyO3, 개발 중)
├── openclaw-plugin/      # OpenClaw 기억 플러그인
├── Dockerfile            # 자체 호스팅 Docker 이미지
└── Cargo.toml            # 워크스페이스 루트
```

## HTTP API

Cortex는 어떤 언어나 프레임워크와도 통합할 수 있는 경량 HTTP 서버를 제공합니다. 기본적으로 `127.0.0.1`에 바인딩 — 데이터가 절대 기기를 벗어나지 않습니다.

```bash
# 빌드 및 실행
cargo build --release -p cortex-http
./target/release/cortex-http --port 3315 --db ~/.cortex/memory.db

# 또는 Docker를 통해 (GHCR에서 사전 빌드된 이미지)
docker run -v ~/.cortex:/data -p 3315:3315 ghcr.io/gambletan/cortex/cortex-http:latest

# 또는 로컬 빌드
docker build -t cortex .
docker run -v ~/.cortex:/data -p 3315:3315 cortex
```

### 엔드포인트

| 메서드 | 경로 | 설명 |
|--------|------|-------------|
| GET | `/health` | 헬스 체크 |
| POST | `/v1/memories` | 기억 저장 |
| POST | `/v1/memories/search` | 시맨틱 검색 |
| GET | `/v1/memories/context` | LLM 컨텍스트 생성 |
| POST | `/v1/memories/consolidate` | 통합 사이클 실행 |
| POST | `/v1/memories/infer` | 추론 미리보기 (저장 안 함) |
| POST | `/v1/facts` | 시맨틱 사실 추가 |
| POST | `/v1/facts/contradictions` | 모순 확인 |
| POST | `/v1/preferences` | 선호도 설정 |
| GET | `/v1/beliefs` | 신뢰 목록 조회 |
| POST | `/v1/beliefs/observe` | 증거로 신뢰 업데이트 |
| POST | `/v1/people` | 사람 신원 해석 |
| POST | `/v1/memories/compress` | 이전 대화 세션 압축 |
| POST | `/v1/relationships/extract` | 텍스트에서 관계 추출 |
| GET | `/v1/export` | 모든 데이터 내보내기 (JSON 백업) |
| POST | `/v1/import` | 백업에서 데이터 가져오기 |

### 예시

```bash
# 기억 저장
curl -X POST http://localhost:3315/v1/memories \
  -H 'Content-Type: application/json' \
  -d '{"text": "다크 모드를 선호합니다", "channel": "cli"}'

# 검색
curl -X POST http://localhost:3315/v1/memories/search \
  -H 'Content-Type: application/json' \
  -d '{"query": "preferences", "limit": 5}'

# 모든 데이터 내보내기 (iCloud, NAS 등에 백업)
curl http://localhost:3315/v1/export > ~/iCloud/cortex-backup.json

# 백업에서 가져오기
curl -X POST http://localhost:3315/v1/import \
  -H 'Content-Type: application/json' \
  -d @~/iCloud/cortex-backup.json
```

## 로드맵

- **v0.2** ✅ — 로컬 임베딩 통합 (all-MiniLM-L6-v2/ONNX), 배치 쿼리, 중요도 인식 감쇠 + 자동 통합
- **v0.3** ✅ — 능동적 추론 (사실 자동 추출), 시간 인식, 모순 감지, 중국어 NLP
- **v0.4** ✅ — HTTP REST API (axum), 가져오기/내보내기 (JSON 백업), Docker 패키징
- **v0.5** ✅ — 대화 압축, 관계 추론 (영어 + 중국어), 시간 검색 개선, 112개 테스트
- **v1.0** ✅ — 기능 비교 표, 벤치마크 업데이트, 18개 기능 Cortex vs Mem0 vs OpenAI
- **v1.1** ✅ — HNSW 벡터 인덱스 (5만 건 검색: 12ms → 91µs), Python SDK (`pip install cortex-ai-memory`)
- **v1.2** ✅ — 부정 감지 (영어 + 중국어), 다단계 검색, 117개 테스트
- **v1.3** ✅ — 컨텍스트 품질 최적화, 쿼리 확장, 양방향 관계, 126개 테스트
- **v1.4** ✅ — 증분 HNSW, SQL 인덱스 엔터티 쿼리, LLM 요약 훅, 18개 MCP 도구, 설정 가능한 감쇠, LLM 보조 추론, 131개 테스트
- **v1.5** ✅ — Docker 이미지 (GHCR 자동 배포), 배치 저장, 중복 제거, 네임스페이스 격리, 플러그인 시스템, 이벤트 버스, 아카이빙, 351개 테스트
- **v1.6** ✅ — Int8 양자화 (저장 공간 75% 절감), 구체화된 컬럼 인덱스, FTS5 트리거, LRU 캐시 (MemObject + entity-facts), rayon 병렬 감쇠, Arc 임베딩, 세대 기반 캐시 무효화, 25개 MCP 도구, 배치 추론, 향상된 중국어 NLP
- **v1.7** ✅ — **클라우드 동기화** (변경 로그 기반, HLC 순서, LWW 병합), **AES-256-GCM 암호화** (Argon2id KDF), **프라이버시 적용** (Private/Shared/Public), **zeroize** (메모리 초기화), SECURITY.md, 27개 MCP 도구, 400개+ 테스트
- **v2.0** — 새 기기를 위한 스냅샷 부트스트랩, 파일시스템 감시자 (즉시 동기화), 백그라운드 동기화 스레드, 모바일 대상 (iOS/Android)

---

## 라이선스

MIT
