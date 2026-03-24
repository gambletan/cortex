# Cortex

[![GitHub stars](https://img.shields.io/github/stars/gambletan/cortex?style=social)](https://github.com/gambletan/cortex/stargazers)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

[English](README.md) | [中文](README_CN.md) | [한국어](README_KO.md)

### プライベート。無料。ローカル。 — 個人 AI エージェントのためのメモリエンジン。

**あなたの AI のメモリはあなたのデバイスに保存されます — 外部に送信されず、課金されず、監視されません。** 純粋な Rust 製。3.8MB。サードパーティサーバー不要。ゼロテレメトリ。ゼロコスト。自分のクラウドストレージで同期。

> **理念：** あなたの記憶はあなたのものです — クラウドプロバイダーのトレーニングデータでも、スタートアップの収益化資産でも、政府の監視対象でもありません。Cortex はあなた自身のハードウェア上で 100% 動作し、あなた自身のデータベースにすべてを保存し、あなた自身のクラウドストレージ（iCloud、Google Drive、OneDrive、Dropbox）を通じてのみ同期します。仲介者があなたのデータを見ることは一切ありません。API キーは不要。アカウント作成も不要。AI エージェントに接続するだけで、プライベートに、永続的に、サブミリ秒の速度で記憶し続けます。

LLM はセッションのたびにゼロから始まります。あなたのアシスタントはあなたの名前を、好みを、昨日の会話を、先週の決定を忘れてしまいます。現在の「メモリ」ソリューションはプレーンテキストファイル、キーワード検索、あるいはクラウド API であり、200〜500ms のレイテンシを追加し、利用料を請求し、あなたの個人データを他者のサーバーに送信します。

Cortex はこの問題を解決します。Cortex はあなたの AI に、セッション・チャンネル・コンテキストをまたいで永続する、構造化・クエリ可能・自己進化する長期メモリを提供します。ベイズ信念システムによる自己修正、クロスプラットフォームの人物グラフによる ID 統合、すべての操作においてサブミリ秒のパフォーマンス。すべてローカルで動作し、すべてあなたのものです。

### Cortex vs Mem0 vs OpenAI Memory

| | Cortex | Mem0 | OpenAI Memory |
|---|---|---|---|
| **プライバシー** | 100% ローカル、ゼロクラウド | クラウド API（データは相手のサーバーに） | OpenAI サーバー |
| **レイテンシ** | **156µs** インジェスト、**568µs** 検索 | ~200-500ms | ~300-800ms |
| **コスト** | 永久無料 | $99+/月（Pro） | ChatGPT Plus（$20/月） |
| **メモリ階層** | 4 層（ワーキング/エピソード/セマンティック/プロシージャル） | 1 層（フラット） | 1 層（フラット） |
| **ベイズ信念** | 証拠に基づく自己修正 | なし | なし |
| **人物グラフ** | クロスチャンネルの ID 解決 | 有料プランのみ | なし |
| **会話圧縮** | セッションの自動要約 | なし | なし |
| **関係推論** | パターンベース（英語 + 日本語対応） | なし | なし |
| **時間的検索** | 意図認識（「最近」/「初めて」） | なし | なし |
| **矛盾検出** | 信頼スコア付き自動検出 | なし | なし |
| **統合** | エピソード → セマンティックへの自動昇格 | なし | なし |
| **コンテキスト注入** | トークン予算制御の LLM 対応出力 | 手動 | 自動だが不透明 |
| **インポート/エクスポート** | 完全 JSON バックアップ & リストア | API のみ | エクスポート不可 |
| **セルフホスト** | ネイティブバイナリ、Docker、MCP | クラウドのみ | クラウドのみ |
| **バイナリサイズ** | 3.8 MB | npm パッケージ | N/A |
| **依存関係** | ランタイム依存ゼロ | Node.js + クラウド | N/A |
| **オープンソース** | MIT | 一部公開 | 非公開 |
| **暗号化** | AES-256-GCM 暗号化同期（オプション） | なし | なし |
| **プライバシーレベル** | Private（デフォルト、非同期）/ Shared / Public | なし | なし |
| **ゼロテレメトリ** | 分析なし、外部送信なし、検証可能 | 不明 | なし |
| **コスト** | 永久無料、無制限 | $99+/月（Pro） | $20/月（Plus） |
| **中国語 NLP** | ネイティブ対応（推論、検索、関係） | なし | 限定的 |
| **名前空間分離** | ユーザー/コンテキスト別のメモリ分離 | なし | なし |
| **プラグインシステム** | インジェスト/検索/統合のコンパイル時フック | なし | なし |
| **MCP ツール** | Claude/LLM 連携用 25 ツール | サードパーティ | N/A |

### パフォーマンスベンチマーク

| 操作 | Cortex | Mem0（クラウド） | ファイルベース |
|-----------|--------|-------------|------------|
| インジェスト | **156µs** | ~200ms | ~1ms |
| 検索（top-10） | **568µs** | ~300ms | ~10ms |
| コンテキスト生成 | **621µs** | ~500ms | 手動 |
| 信念更新 | **66µs** | N/A | N/A |
| 人物グラフ | **51µs** | 有料プラン | N/A |
| 構造化事実 | **45µs** | N/A | N/A |
| 1K 件のメモリ検索 | **1.6ms** | ~500ms | ~50ms |

Mem0 クラウドより **528 倍高速**。Mem0 も OpenAI Memory も提供していない機能付きで。

> **注記：** ベンチマークにはすべてのインジェスト時に自動実行されるプロアクティブ推論（事実・好み・関係の自動抽出）が含まれます。推論なしの生インジェストは ~15µs です。数値は M シリーズ Mac 上での `cargo bench` の結果です。

### LoCoMo ベンチマーク（[ACL 2024](https://snap-research.github.io/locomo/)）

学術グレードの長期会話メモリ評価 — 10 会話、1540 個の QA ペア、4 カテゴリ。

| システム | 単一ホップ | 多段ホップ | オープンドメイン | 時間推論 | 総合 |
|--------|-----------|-----------|-------------|----------|---------|
| Backboard | 89.4% | 75.0% | 91.2% | 91.9% | 90.0% |
| MemMachine v0.2 | — | — | — | — | 84.9% |
| **Cortex v1.7** | **72.5%** | **59.5%** | **88.8%** | **74.1%** | **73.7%** |
| Mem0-Graph | 65.7% | 47.2% | 75.7% | 58.1% | 68.4% |
| Mem0 | 67.1% | 51.2% | 72.9% | 55.5% | 66.9% |
| OpenAI Memory | — | — | — | — | 52.9% |

**主な結果：**
- **オープンドメイン 88.8%** — Mem0（72.9%）を +15.9% リード
- **時間推論 74.1%** — Mem0（55.5%）を +18.6% リード
- **単一ホップ 72.5%** — Mem0（67.1%）を +5.4% リード
- **多段ホップ 59.5%** — Mem0（51.2%）を +8.3% リード
- **総合 73.7%** — Mem0（66.9%）を +6.8% 上回り、OpenAI Memory（52.9%）を +20.8% 上回る

Cortex は全 4 カテゴリで Mem0 を上回ります — 100% ローカル動作、エンドツーエンド暗号化、コスト $0 で。

> **テスト構成：** Claude Sonnet 4（QA + 評価）、nomic-embed-text（Ollama 経由の埋め込み）、top-30 検索。完全再現可能：`python3 bench/locomo_bench.py`

## アーキテクチャ

Cortex は人間の認知にヒントを得た 4 層メモリモデルを実装しています：

```
                    +---------------------+
                    |   Working Memory    |  現在のセッションコンテキスト
                    +---------------------+
                              |
                    +---------------------+
                    |   Episodic Memory   |  生の体験：会話、イベント、観察
                    +---------------------+
                              |  統合（減衰、昇格、パターン抽出）
                    +---------------------+
                    |   Semantic Memory   |  抽出された事実、好み、関係
                    +---------------------+
                              |
                    +---------------------+
                    | Procedural Memory   |  学習済みルーティン、ユーザー固有のワークフロー
                    +---------------------+
```

**ワーキングメモリ**は現在のセッションのスクラッチパッドを保持します。**エピソードメモリ**はタイムスタンプとソースメタデータ付きの生体験を保存します。**統合エンジン**は定期的に繰り返しパターンを**セマンティック**事実へ昇格させ、陳腐化したエピソードを減衰させます。**プロシージャルメモリ**は学習済みのワークフローとルーティンを記録します。

## 主要コンポーネント

### 人物グラフ
クロスチャンネルの ID 解決。Telegram でメッセージを送り、メールを送り、カレンダーイベントに登場する同一人物を、単一の ID ノードに統合します。インタラクション、関係強度、コミュニケーションパターンを人物ごとに追跡します。

### ベイズ信念システム
自己修正する世界の理解。信念は証拠から形成され、新しい観察のたびに更新され、反証によって覆されます。信頼スコアは時効バイアスではなく、実際の確実性を反映します。

```rust
cortex.observe_belief("user_prefers_morning_meetings", true, 0.8)?;
cortex.observe_belief("user_prefers_morning_meetings", false, 0.6)?;
// 信頼度はベイズ更新によって自動的に調整される
```

### 統合エンジン
エピソードからセマンティックへの昇格、陳腐化したメモリの減衰、パターン抽出。メモリストアを軽量かつクエリ可能な状態に保つバックグラウンドサイクルとして動作します。昇格・減衰・マージの詳細レポートを返します。

### マルチシグナル検索
クエリは関連性ランキングのために 5 つのシグナルを組み合わせます：
- **類似度** — クエリ埋め込みに対するベクトルコサイン距離
- **時間的** — 設定可能な減衰による最新性の重み付け
- **顕著性** — アクセスパターンと明示的なヒントによる重要度スコア
- **ソーシャル** — 特定の人物に関連するメモリのブースト
- **チャンネル** — ソースチャンネルによるフィルタリングまたはブースト

### コンテキスト注入プロトコル
メモリ状態から LLM 対応のコンテキスト文字列を生成します。トークン予算、オプションのチャンネル/人物フィルタを渡すと、LLM が直接使用できる構造化テキストブロックが返されます。

### ストレージ
永続化に SQLite、高速類似度検索にインメモリベクトルインデックスを使用します。単一ファイルデータベース、外部サービス不要。エッジデプロイ向けに設計 — ラップトップ、Raspberry Pi、サーバーいずれでも動作します。

### クラウド同期

自分のクラウドストレージを通じてデバイス間でメモリを同期 — サードパーティサーバーは一切不要。

```
デバイス A (Mac)              自分のクラウドストレージ              デバイス B (iPhone)
┌──────────┐         ┌──────────────────────┐         ┌──────────┐
│ SQLite DB │ ──W──>  │ iCloud / GDrive /    │  <──R── │ SQLite DB│
│ (ローカル) │         │ OneDrive / Dropbox   │         │ (ローカル)│
│           │ <──R──  │                      │  ──W──> │          │
└──────────┘         └──────────────────────┘         └──────────┘
```

- **変更ログベース**：各デバイスは自分専用のサブフォルダに追記専用の操作ログを書き込む
- **競合なし**：デバイスは同じファイルに書き込まない。ハイブリッド論理クロック (HLC) を用いた Last-Writer-Wins でマージ
- **暗号化**：AES-256-GCM 暗号化（オプション）。クラウドアカウントが侵害されても、メモリはプライベートを保つ
- **プライバシー対応**：Private メモリ（デフォルト）はデバイスから外に出ない。Shared/Public メモリのみ同期

対応プロバイダー：**iCloud Drive**、**Google Drive**、**OneDrive**、**Dropbox**（自動検出）。

```rust
use cortex_core::sync::SyncConfig;

// 暗号化同期を有効化
let config = SyncConfig::new(sync_dir, device_id, device_name)
    .with_encryption("my-strong-passphrase");
cortex.enable_sync(config)?;

// 他デバイスの変更を取得
let applied = cortex.sync_pull()?;
println!("Applied {} remote changes", applied);
```

### セキュリティとプライバシー

| 機能 | 詳細 |
|---------|--------|
| **暗号化** | AES-256-GCM + Argon2id 鍵導出（行ごとのランダム nonce） |
| **プライバシーレベル** | Private（デフォルト、非同期）、Shared、Public |
| **メモリのゼロ化** | ドロップ時に機密データを RAM から消去（`zeroize` クレート） |
| **ゼロテレメトリ** | 分析なし、外部送信なし。確認：`grep -r "reqwest\|hyper\|TcpStream" cortex-core/src/` |
| **アカウント不要** | API キー不要、登録不要、クラウド依存なし |

完全な脅威モデルは [SECURITY.md](SECURITY.md) を参照してください。

## 前提条件

[Rust ツールチェーン](https://rustup.rs/)（`cargo` を提供）をインストールします：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

インストール後、ターミナルを再起動するか以下を実行してください：
```bash
source "$HOME/.cargo/env"
```

確認：
```bash
cargo --version
```

## 実際の使用例：本当に記憶する AI アシスタント

1 週間の実際の会話での AI アシスタントの動作を想像してください：

```
# 1 日目 — Telegram でチャット
あなた：「Stripe の Sarah との会議はうまくいった。彼女は私たちの API に興味を持っている。」

  Cortex が自動抽出：
  ├── エピソードメモリ保存済み（156µs）
  ├── 事実：Sarah → works_at → Stripe（信頼度：0.85）
  ├── 事実：Sarah → interested_in → 私たちの API
  └── 人物解決：sarah_telegram

# 2 日目 — Sarah からメール
From: sarah@stripe.com
「話し合った技術仕様書を送ります。」

  Cortex：
  ├── 人物解決：sarah@stripe.com → sarah_telegram にマージ
  │   （同一人物、異なるチャンネル — 自動 ID 解決）
  └── 事実：Sarah → sent → 技術仕様書

# 3 日目 — AI に質問
あなた：「Stripe との状況はどうなっている？」

  Cortex が検索（568µs）：
  ├── Sarah は Stripe に勤務（セマンティック事実）
  ├── 会議は順調、API に興味（エピソード、1 日目）
  ├── 技術仕様書を送ってきた（エピソード、2 日目）
  └── クロスチャンネルコンテキスト：Telegram + Email が同一人物として統合

  AI が完全なコンテキストで応答 — 「覚えていません」とはもう言わない

# 5 日目 — 新しい情報
あなた：「実は Sarah は先月 Anthropic に転職したらしい。」

  Cortex：
  ├── 矛盾検出：Sarah works_at Stripe vs Sarah works_at Anthropic
  ├── 旧事実の信頼度を減衰：Stripe（0.85 → 0.15）
  ├── 新事実を保存：Sarah → works_at → Anthropic（0.90）
  └── ベイズ推論で信念を自動更新 — 自己修正、手動クリーンアップ不要

# 7 日目 — 統合が実行
  Cortex の自動統合：
  ├── Sarah に関する 3 件のエピソードメモリ → セマンティックサマリーに昇格
  ├── 他のトピックの陳腐化したメモリ → 減衰
  └── パターン検出：毎週月曜日に定例会議がある
```

これらはすべて **ローカルで 1 操作あたり <1ms** で実行されます。クラウドなし。API 呼び出しなし。誰もあなたのデータを見ません。

## クイックスタート

```rust
use cortex_core::Cortex;

// メモリデータベースを開く（または作成する）
let cortex = Cortex::open("memory.db")?;

// Telegram の会話からメモリをインジェスト
let embedding = your_embedding_fn("Alice と Q3 ロードマップについて話し合った");
cortex.ingest(
    "Alice と Q3 ロードマップについて話し合った",
    "telegram",               // ソースチャンネル
    Some("alice_123"),         // ユーザー ID（ID 解決をトリガー）
    Some(0.8),                 // 顕著性ヒント
    Some(embedding),           // ベクトル埋め込み
)?;

// セマンティック事実を直接追加
cortex.add_fact(
    "Alice", "works_at", "Acme Corp",
    0.95, "telegram", None,
)?;

// 好みを保存
cortex.add_preference("timezone", "Asia/Tokyo", 0.9)?;

// 関連するメモリを検索
let results = cortex.retrieve(
    "Alice について何を知っているか？",
    5,                         // top-k
    None,                      // 任意のチャンネル
    None,                      // 任意の人物
    Some(query_embedding),     // 類似度検索用ベクトル
)?;

// LLM 対応コンテキストを生成（トークン予算制御）
let context = cortex.get_context(
    2000,                      // 最大トークン数
    Some("telegram"),          // チャンネルフィルター
    None,                      // 人物フィルターなし
)?;
// `context` を LLM のシステム/ユーザーメッセージのプレフィックスとして渡す

// 統合を実行（定期的に呼び出す）
let report = cortex.run_consolidation()?;
println!("Promoted: {}, Decayed: {}", report.promoted, report.decayed);
```

## Python バインディング

[PyO3](https://pyo3.rs) 経由で近日公開予定。`cortex-python` クレートは全 API をネイティブ Python モジュールとして提供します：

```python
from cortex import Cortex

cx = Cortex.open("memory.db")
cx.ingest("Bob とタイ料理店でランチした", channel="imessage", user_id="bob")
results = cx.retrieve("Bob はどこで食事するのが好きか？", limit=5)
```

## unified-channel-hub との統合

Cortex は [unified-channel-hub](https://github.com/gambletan/unified-channel-hub) のメモリレイヤーとして設計されています。任意のチャンネルアダプターからメッセージが流入し、Cortex がそれをインジェストしてインデックスを作成し、コンテキスト注入プロトコルが各レスポンスの前に関連メモリを LLM にフィードバックします。

```
Telegram ─┐                          ┌─ Context
Discord  ─┤  unified-channel-hub  →  │  Cortex  →  LLM
Email    ─┤  (ingest)                 │  (retrieve + inject)
Calendar ─┘                          └─ Response
```

## MCP サーバー（Claude Code / Claude Desktop）

Cortex は MCP サーバーとして提供され、MCP 対応のクライアントであれば何でも使用できます。

### セットアップ

**1. バイナリをビルドしてインストール：**

```bash
mkdir -p ~/.local/bin ~/.cortex
cargo build --release -p cortex-mcp-server
cp target/release/cortex-mcp-server ~/.local/bin/
```

**2. MCP サーバーとして登録：**

Claude Code（CLI）：
```bash
# グローバル（全プロジェクト共通）
claude mcp add cortex --scope user -- ~/.local/bin/cortex-mcp-server ~/.cortex/memory.db

# またはプロジェクト単位
claude mcp add cortex -- ~/.local/bin/cortex-mcp-server ~/.cortex/memory.db
```

Claude Desktop — `~/Library/Application Support/Claude/claude_desktop_config.json` に追加：
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

**3. ツールを「確認なし」モードで許可：**

`~/.claude/settings.json` → `permissions.allow` に追加：
```json
"mcp__cortex__*"
```

> 注意：MCP ツールの権限は括弧形式（例：`mcp__cortex__memory_ingest(*)`）をサポートしていません。代わりにワイルドカード `mcp__cortex__*` を使用してください。

**4. 自動メモリを有効にする** — `CLAUDE.md`（プロジェクトまたはグローバルの `~/.claude/CLAUDE.md`）に追加：

```markdown
# メモリ（Cortex）
Cortex MCP ツールを通じて永続的なメモリを持っています。以下を自動的に使用してください：
- 会話の開始時：`memory_context` を呼び出してユーザーに関する情報をロード
- ユーザーが好み・事実・個人情報を共有した時：`memory_ingest` を呼び出して保存
- 構造化事実を学んだ時：`fact_add` を呼び出す（例：「User works_at Google」）
- 好みを検出した時：`preference_set` を呼び出す（例：editor=neovim）
- 証拠が信念を支持または矛盾する時：`belief_observe` を呼び出す
- 新しい人物と話す時：`person_resolve` を呼び出して ID を追跡
- 定期的に：`memory_consolidate` を呼び出して陳腐化したメモリをクリーンアップ
```

**5. セッション開始時にメモリを自動注入**（Claude Code フック — 完全自動）：

`~/.claude/hooks/cortex-memory-inject.sh` を作成：
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

`~/.claude/settings.json` に追加：
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

これで新しい Claude Code セッションのたびにメモリコンテキストが自動的にロードされます — **手動操作ゼロ**。Claude は作業しながら学習し、セッションをまたいで記憶し続けます。

### マルチプロジェクトの分離

複数のプロジェクトをまたいで作業していますか？物理的なメモリ分離には**別々のデータベース**を使用します — クロスプロジェクトの漏洩ゼロ、コード変更不要。

```
~/.cortex/
├── global.db          # ユーザー設定、人物グラフ、プロジェクト横断の知識
├── my-app.db          # プロジェクト A のメモリ
└── my-api.db          # プロジェクト B のメモリ
```

**グローバル設定**（`~/.claude/settings.json`）— ユーザーレベルの知識：
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

**プロジェクト設定**（`~/.claude/projects/<path>/settings.json`）— プロジェクト固有：
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

プロジェクトの `CLAUDE.md` に以下のメモリ分離ルールを追加します：

```markdown
## メモリ分離

2 つの Cortex MCP サーバー：`cortex-project`（プロジェクト DB）と `cortex-global`（グローバル DB）。

### 書き込みポリシー
- このリポジトリのアーキテクチャ、コード、モジュール、テスト、ワークフロー、設定、バグ、決定、用語に関するメモリは `cortex-project` に保存。
- 長期的なユーザー設定、コミュニケーションスタイル、プロジェクト横断の習慣、リポジトリをまたいで有用な個人的背景のみ `cortex-global` に保存。
- **デフォルト：不明な場合は `cortex-project` に保存。**

### 読み取りポリシー
1. まず `cortex-project` を照会する。
2. ユーザーレベルの設定のみを目的として `cortex-global` を照会する。
3. 競合する場合はプロジェクトメモリを優先する。

### 漏洩防止ルール
- `cortex-project` から `cortex-global` へ自動的にコピーしない。
- リポジトリ固有のパス、モジュール名、アカウント名を `cortex-global` に保存しない。
- プロジェクトの実装詳細をユーザーグローバルの設定として扱わない。

### 更新ルール
- Cortex は追記専用です。更新する場合：古いエントリを検索 → 削除 → 新しいエントリをインジェスト。
```

これによりプロジェクトごとに 2 つの独立した Cortex インスタンスが得られます — 完全な分離と共有ユーザー知識を両立。

### 27 のツール

| ツール | 目的 |
|------|---------|
| `memory_ingest` | メモリを保存（テキスト、チャンネル、人物コンテキスト） |
| `memory_search` | 全メモリ階層をまたいだセマンティック検索 |
| `memory_context` | LLM 対応のコンテキストサマリーを生成（トークン予算制御） |
| `memory_consolidate` | 減衰 + 昇格 + スイープサイクルを実行 |
| `memory_infer` | 保存せずに推論をプレビュー |
| `memory_compress` | 古い会話セッションを圧縮 |
| `memory_stats` | メモリ統計を取得（階層別件数、インデックスサイズ） |
| `memory_decay` | エピソードメモリに時間的減衰を適用 |
| `belief_observe` | 証拠でベイズ信念を更新 |
| `belief_list` | 信頼閾値を超える信念を照会 |
| `fact_add` | 構造化知識を保存（主語-述語-目的語） |
| `fact_query` | エンティティ別に事実を照会（SQL インデックス） |
| `preference_set` | 信頼度付きでユーザー設定を保存 |
| `preference_query` | キーパターンで設定を照会 |
| `person_resolve` | クロスチャンネルの ID 解決 |
| `person_list` | 既知の全人物を一覧表示 |
| `contradiction_check` | 事実の矛盾を確認 |
| `relationship_extract` | テキストから関係を抽出 |
| `sync_status` | クラウド同期の状態（プロバイダー、デバイス、保留中の操作） |
| `sync_providers` | 利用可能なクラウドストレージプロバイダーを検出 |

## OpenClaw プラグイン

OpenClaw エージェントに自動記憶と自動呼び出しによる永続メモリを追加します。

**インストール：**

```bash
# 1. Cortex バイナリをインストール
curl -fsSL https://raw.githubusercontent.com/gambletan/cortex/main/install.sh | bash

# 2. OpenClaw プラグインをインストール
openclaw plugin add @cortex-ai-memory/cortex-memory
```

**設定**（任意 — デフォルトで動作します）：

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

**機能：**
- `autoCapture`：各ターン後に会話コンテキストを自動保存
- `autoRecall`：各ターン前に関連メモリを自動注入（エージェントが「記憶」する）
- 7 つのツール：memory_search、memory_store、fact_add、belief_observe、person_resolve など

詳細は `openclaw-plugin/README.md` を参照してください。

## プロジェクト構成

```
cortex/
├── cortex-core/          # Rust コアライブラリ（全メモリロジック）
│   ├── src/
│   │   ├── lib.rs              # Cortex エントリポイント
│   │   ├── types.rs            # MemObject、MemoryTier など
│   │   ├── inference.rs        # プロアクティブ推論（英語 + 中国語）
│   │   ├── episode.rs          # エピソードメモリストア
│   │   ├── semantic.rs         # セマンティック事実 + 設定
│   │   ├── working.rs          # ワーキングメモリ（セッションスクラッチパッド）
│   │   ├── procedural.rs       # 学習済みルーティン
│   │   ├── people.rs           # 人物グラフ + ID 解決
│   │   ├── belief.rs           # ベイズ信念システム
│   │   ├── consolidation.rs    # エピソード → セマンティック昇格 + 減衰
│   │   ├── retrieval.rs        # マルチシグナル検索エンジン
│   │   ├── context.rs          # LLM コンテキスト生成
│   │   ├── sync/               # クラウド同期（oplog、HLC、マージ、暗号化）
│   │   └── storage/            # SQLite + インメモリベクトルインデックス
│   └── benches/                # パフォーマンスベンチマーク
├── cortex-http/          # HTTP REST API（axum、ローカルのみ）
├── cortex-mcp-server/    # MCP サーバーバイナリ（3.8MB）
├── cortex-python/        # Python バインディング（PyO3、WIP）
├── openclaw-plugin/      # OpenClaw メモリプラグイン
├── Dockerfile            # セルフホスト用 Docker イメージ
└── Cargo.toml            # ワークスペースルート
```

## HTTP API

Cortex は任意の言語やフレームワークとの統合のための軽量 HTTP サーバーを提供します。デフォルトで `127.0.0.1` にバインド — あなたのデータはマシンの外に出ません。

```bash
# ビルドして実行
cargo build --release -p cortex-http
./target/release/cortex-http --port 3315 --db ~/.cortex/memory.db

# または Docker 経由（GHCR のプレビルドイメージ）
docker run -v ~/.cortex:/data -p 3315:3315 ghcr.io/gambletan/cortex/cortex-http:latest

# またはローカルでビルド
docker build -t cortex .
docker run -v ~/.cortex:/data -p 3315:3315 cortex
```

### エンドポイント

| メソッド | パス | 説明 |
|--------|------|-------------|
| GET | `/health` | ヘルスチェック |
| POST | `/v1/memories` | メモリをインジェスト |
| POST | `/v1/memories/search` | セマンティック検索 |
| GET | `/v1/memories/context` | LLM コンテキストを生成 |
| POST | `/v1/memories/consolidate` | 統合サイクルを実行 |
| POST | `/v1/memories/infer` | 推論をプレビュー（保存なし） |
| POST | `/v1/facts` | セマンティック事実を追加 |
| POST | `/v1/facts/contradictions` | 矛盾を確認 |
| POST | `/v1/preferences` | 設定を保存 |
| GET | `/v1/beliefs` | 信念を一覧表示 |
| POST | `/v1/beliefs/observe` | 証拠で信念を更新 |
| POST | `/v1/people` | 人物 ID を解決 |
| POST | `/v1/memories/compress` | 古い会話セッションを圧縮 |
| POST | `/v1/relationships/extract` | テキストから関係を抽出 |
| GET | `/v1/export` | 全データをエクスポート（JSON バックアップ） |
| POST | `/v1/import` | バックアップからデータをインポート |

### 使用例

```bash
# メモリを保存
curl -X POST http://localhost:3315/v1/memories \
  -H 'Content-Type: application/json' \
  -d '{"text": "ダークモードが好きです", "channel": "cli"}'

# 検索
curl -X POST http://localhost:3315/v1/memories/search \
  -H 'Content-Type: application/json' \
  -d '{"query": "preferences", "limit": 5}'

# 全データをエクスポート（iCloud、NAS などにバックアップ）
curl http://localhost:3315/v1/export > ~/iCloud/cortex-backup.json

# バックアップからインポート
curl -X POST http://localhost:3315/v1/import \
  -H 'Content-Type: application/json' \
  -d @~/iCloud/cortex-backup.json
```

## ロードマップ

- **v0.2** ✅ — ローカル埋め込み統合（all-MiniLM-L6-v2/ONNX）、バッチクエリ、重要度対応の減衰 + 自動統合
- **v0.3** ✅ — プロアクティブ推論（事実の自動抽出）、時間認識、矛盾検出、中国語 NLP
- **v0.4** ✅ — HTTP REST API（axum）、インポート/エクスポート（JSON バックアップ）、Docker パッケージング
- **v0.5** ✅ — 会話圧縮、関係推論（英語 + 中国語）、時間的検索強化、112 テスト
- **v1.0** ✅ — 機能比較表、ベンチマーク更新、18 機能の Cortex vs Mem0 vs OpenAI 比較
- **v1.1** ✅ — HNSW ベクトルインデックス（5 万件検索：12ms → 91µs）、Python SDK（`pip install cortex-ai-memory`）
- **v1.2** ✅ — 否定検出（英語 + 中国語）、マルチホップ検索、117 テスト
- **v1.3** ✅ — コンテキスト品質最適化、クエリ展開、双方向関係、126 テスト
- **v1.4** ✅ — 増分 HNSW、SQL インデックスエンティティクエリ、LLM サマライザーフック、18 MCP ツール、設定可能な減衰、LLM 支援推論、131 テスト
- **v1.5** ✅ — Docker イメージ（GHCR 自動公開）、バッチインジェスト、重複排除、名前空間分離、プラグインシステム、イベントバス、アーカイブ、351 テスト
- **v1.6** ✅ — Int8 量子化（ストレージ 75% 削減）、マテリアライズドカラムインデックス、FTS5 トリガー、LRU キャッシュ（MemObject + entity-facts）、rayon 並列減衰、Arc 埋め込み、世代ベースのキャッシュ無効化、25 MCP ツール、バッチ推論、強化された中国語 NLP
- **v1.7** ✅ — **クラウド同期**（変更ログベース、HLC 順序付け、LWW マージ）、**AES-256-GCM 暗号化**（Argon2id KDF）、**プライバシー強制**（Private/Shared/Public）、**zeroize**（メモリ消去）、SECURITY.md、27 MCP ツール、400+ テスト
- **v2.0** — 新デバイス向けスナップショットブートストラップ、ファイルシステムウォッチャー（即時同期）、バックグラウンド同期スレッド、モバイル対応（iOS/Android）

---

## ライセンス

MIT
