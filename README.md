# Sirucord

[![Test and release](https://github.com/hama6767/sirucord/actions/workflows/ci.yml/badge.svg)](https://github.com/hama6767/sirucord/actions/workflows/ci.yml)

**Discord内のGo Live・画面共有を見つけて、配信者と配信内容をMastodonへ通知するRustアプリです。** GitHub Actionsだけで動かせます。常設サーバーやデータベースは不要です。

```text
🔴 はま さんのDiscord配信開始を検知しました
内容：Minecraftで街づくり
https://discord.com/channels/サーバーID/ボイスチャンネルID
```

## できること

- 指定したボイスチャンネルで配信している人を自動検出。配信者の個別登録は不要。
- 配信者のサーバー内表示名、本人が書いた配信説明、チャンネルへのリンクを投稿。任意設定でDiscordのプレイ中ゲーム名も利用可能。
- 本人が専用テキストチャンネルへ送ったスクリーンショットを、既定で30分間隔までに抑えて転送。
- 同じ配信の二重通知を抑止。終了を確認した後の再配信は新しく通知。
- 通知履歴を暗号化してGitHubの専用ブランチへ保存。Actionsのキャッシュ失効による履歴消失を回避。
- Windows / Linuxバイナリ、通信を伴わない設定チェック、投稿しない接続テスト。

## 先に知っておく制約

| 項目 | 動作 |
|---|---|
| 対象 | Discordサーバー内のGo Live・画面共有。カメラだけの通話やTwitch/YouTubeは対象外 |
| 通知の速さ | 5分間隔の確認。Actionsの混雑でさらに遅延・欠落する場合あり |
| 短い配信 | 確認と確認の間に開始・終了した配信は検知できない |
| 再配信 | 同じボイス接続のまま、確認間隔内に停止・再開した場合は区別できない |
| 初回起動 | すでに配信中の人も最初の1回は通知する。正確な開始時刻は取得しない |
| 配信内容 | 本人の説明 → 任意のプレイ中ゲーム表示 → `default_title` の順で使用。映像や共有ウィンドウ名は取得しない |
| スクリーンショット | **Discord映像の自動撮影は非対応**。本人が送信した画像を自動転送する |
| 完全放置 | 公開リポジトリは60日間アクティビティがないと定期実行が自動停止する。再有効化が必要になる場合あり |

Discordの公開APIにはGo Live映像をスクリーンショットとして取得するエンドポイントがありません。このため通常のBotで扱えるメタデータと、本人が明示的に送った画像を使います。ユーザーアカウントのトークンや非公式クライアントは必要ありません。

## GitHub Actionsで始める

### 1. Discord Botを作る

1. [Discord Developer Portal](https://discord.com/developers/applications) でアプリを作成し、**Bot** のトークンを発行します。
2. 配信説明・画像の転送を使う場合、Botの **Message Content Intent** を有効にします。`use_activity = true` を使う場合だけ **Presence Intent** も有効にします。Server Members Intent は不要です。
3. OAuth2 URL Generatorで `bot` スコープを選び、サーバーに招待します。
4. 対象ボイスチャンネルの **View Channel / Connect**、説明用テキストチャンネルの **View Channel / Read Message History** を許可します。管理者権限やメッセージ送信権限は不要です。実際の音声チャンネルへの入室は行いません。
5. Discordの開発者モードをオンにして、サーバーID・対象ボイスチャンネルID・説明用テキストチャンネルIDをコピーします。

通知対象チャンネルの参加者には、配信者名と配信内容がMastodonへ転送されることを知らせてください。サーバー単位の設定では対象チャンネルで配信する全員が通知対象になります。特定の人だけにしたい場合は後述の個別設定を使います。

### 2. Mastodonのトークンを作る

投稿先アカウントで **設定 → 開発 → 新規アプリ** を開き、次の権限を持つアクセストークンを作ります。

```text
read:accounts write:statuses write:media
```

`read:accounts` は起動時の接続チェック、`write:media` は画像投稿に使います。初期設定の `unlisted` は公開範囲です（公開タイムライン掲載を抑制するだけで、秘密にはなりません）。フォロワー向けに限定する場合は `private` を選んでください。

### 3. 設定とSecretsを登録する

[config.example.toml](config.example.toml) をコピーし、IDとMastodonのURLを置き換えます。IDは引用符で囲んだ文字列です。

```toml
screenshot_interval_minutes = 30

[mastodon]
base_url = "https://あなたのMastodonサーバー"
visibility = "unlisted"

[[servers]]
guild_id = "123456789012345678"
voice_channel_ids = ["345678901234567890"]
default_title = "ゲーム・作業の画面共有（詳しくはDiscordで）"
announcement_channel_id = "456789012345678901"
screenshots = true
```

説明・画像を使わない場合は `announcement_channel_id` を削除し、`screenshots = false` にします。説明がない場合に送信される `default_title` は、実際の運用に合わせて設定してください。

`[[servers]]` に `use_activity = true` を追加すると、本人の説明がない場合にDiscordのプレイ中ゲーム名を自動利用します。Presence Intentも有効にしてください。「Discordのプレイ中表示」と明記して投稿します。プレイ中ゲームと実際に共有している画面は一致するとは限りません。アクティビティを非公開にしている人などは `default_title` に戻ります。

GitHubの **Settings → Secrets and variables → Actions → Secrets** に以下を登録します。設定全体もSecretとして扱い、公開リポジトリにIDを含む設定ファイルをコミットする必要はありません。

| Secret | 値 |
|---|---|
| `DISCORD_BOT_TOKEN` | Discord Botトークン |
| `MASTODON_ACCESS_TOKEN` | Mastodonアクセストークン |
| `SIRUCORD_CONFIG` | 編集したTOML全文 |
| `SIRUCORD_STATE_KEY` | 32文字以上のランダムな暗号化キー（トークンとは別に生成） |

暗号化キーの生成例（PowerShell 7）：

```powershell
[Convert]::ToBase64String([System.Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
```

キーはパスワードマネージャー等に保存してください。変更すると既存の通知履歴を復号できなくなります。Discord/Mastodonトークンはキーを維持したまま交換できます。

### 4. 接続確認して有効化する

1. [Releases](https://github.com/hama6767/sirucord/releases) に使うバージョンの配布ファイルがあることを確認します。
2. **Actions → Announce Discord streams → Run workflow** を開き、`dry_run` をオンにして実行します。Discord/Mastodonへの読み取りだけを行い、投稿も履歴保存も行いません。定期監視を有効にする前でも手動実行できます。
3. 接続確認後、**Settings → Secrets and variables → Actions → Variables** に `SIRUCORD_ENABLED` = `true` を登録し、定期監視を有効にします。
4. 配信を開始し、最初の通知を確認します。手動で投稿処理を実行する場合は `dry_run` をオフにします。

`GITHUB_TOKEN` はActionsが自動発行します。個人用GitHubトークンの登録は不要です。ワークフローには `contents: write` が必要です。組織ポリシーやブランチルールで拒否される場合は管理者による設定が必要です。

停止するには `SIRUCORD_ENABLED` を `false` にするか、Actionsでワークフローを無効化します。デプロイ直後は変数が未登録なので外部への投稿は始まりません。

## Discordで配信内容・画像を送る

説明用チャンネルに、**配信者本人のアカウント**で次のように送ります。Botのスラッシュコマンドではなく、通常のメッセージです。

```text
!sirucord Minecraftで街づくり
```

直近12時間・最新100件以内にある本人の説明を使います。配信を始める前に説明を書いておくと、開始通知に反映されます。

スクリーンショットは、**開始通知が届いた後**に `!sirucord` または `!sirucord 配信内容` と書いたメッセージへ添付します。PNG / JPEG / WebP、8 MiB以下を受け付けます。配信が続いていれば、設定間隔が経過した確認時に最新の新しい画像を1枚だけ投稿します。新しい画像がなければ投稿しません。

通常の会話、他の人の画像、Webhook/Bot投稿、配信検知より前の画像は転送しません。本人が `!sirucord` と指定した投稿でも、画面内の個人情報などは送信前に確認してください。Discordが表示する配信サムネイルの自動取得や、PC画面の自動撮影機能はありません。

## 個別の配信者だけを対象にする

同じサーバーの `[[servers]]` を削除し、代わりに以下を設定します。複数人は `[[streamers]]` を繰り返します。このモードはGatewayに接続せず、公式のユーザー音声状態REST APIを利用します。

個別設定では `use_activity` は使用できません。

```toml
[[streamers]]
guild_id = "123456789012345678"
user_id = "234567890123456789"
display_name = "はま"
voice_channel_ids = ["345678901234567890"]
default_title = "ゲーム配信"
announcement_channel_id = "456789012345678901"
screenshots = true
```

## 無料運用とメンテナンス

この公開リポジトリの標準GitHubホストランナーで実行します。監視ごとにRustをビルドせず、リリース済みバイナリをダウンロードしてSHA-256を検証し、1回確認したら終了します。常駐ランナー、課金サービス、外部DBを起動しません。CIの配布用一時Artifactは1日で期限切れになります。

公開リポジトリの標準ランナーの実行時間は無料です。ただしGitHubの料金・利用制限は将来変わり得ます。非公開化した場合の無料実行枠、Artifactやキャッシュのストレージ枠、既存のMastodonアカウントの費用は別途確認してください。依存パッケージとActionsの更新はDependabotが月次で提案しますが、自動マージはしません。

60日間更新がない場合の定期実行停止、API仕様変更、トークン失効、投稿成否が不明な障害への対応は残ります。「常設サーバーの保守不要」を目指した構成で、完全なメンテナンス不要や即時通知を保証するものではありません。

## 障害時の動作と復旧

- Discordの応答が不完全、権限不足、通信障害なら、配信終了と誤判定せず処理を失敗させます。
- 配信情報と送信予定を、`sirucord-state` ブランチの `state.enc` にAES-256-GCMで暗号化して保存してから投稿します。HKDF-SHA256で専用キーを導出し、毎回ランダムなnonceを使います。
- Mastodonの `Idempotency-Key` を永続化して再試行します。成否不明の送信から55分以上経った場合は、重複投稿を避けるため自動送信を停止します。Mastodon側の重複防止キーは最大1時間保持されるためです。
- APIの429/5xxは短い範囲で再試行し、長い待機が必要な場合は次の定期実行へ持ち越します。
- 公開Actionsログには投稿本文・配信者ID・トークンを出しません。ただし実行時刻、配信件数や暗号化履歴の更新時刻は公開されます。

成否不明の投稿は、**先に監視を停止**してMastodonアカウントを確認します。ローカルで同じ設定・暗号化キーと、リポジトリのContents読み書き権限を持つ `GITHUB_TOKEN`、`GITHUB_REPOSITORY=hama6767/sirucord` を環境変数に設定し、次を実行します。

```sh
sirucord --github-state pending
# 投稿済みであることを確認した場合
sirucord --github-state resolve --target GUILD_ID:USER_ID --posted
# 未投稿であることを確認した場合だけ、新しいキーで再試行を許可
sirucord --github-state resolve --target GUILD_ID:USER_ID --retry
```

復旧後に監視を再開します。`--retry` は確認を誤ると重複投稿になります。`pending` の出力にはIDが含まれるため、公開Actionsでは実行しないでください。暗号化履歴を消して再実行する方法は推奨しません。初回の状態ブランチ作成直後に保存が失敗し、`state.enc` がまだない場合は、外部投稿が始まっていないことを確認してから未初期化ブランチを削除し、再初期化してください。

複数プロセスで同じ履歴を同時に更新しないでください。Actionsはconcurrencyで直列化しています。端末からの手動実行・復旧時は定期実行を止めます。画像転送中の障害では未添付メディアがMastodonに残る場合があります。Discordで元画像が削除された場合は画像を再送してください。

## ローカル開発・実行

Rust 1.98.1でCIを実行しています。

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo run --locked -- --config config.example.toml check
cargo build --release --locked
```

`config.example.toml` を `config.toml` へコピーして編集し、前述の4つのSecretに対応する環境変数を設定します。ローカルでは `SIRUCORD_CONFIG` を省略すればファイルから読み込みます。

```sh
sirucord check
sirucord run --dry-run
sirucord run
```

1回だけ実行するアプリです。標準のローカル状態ファイルは `state.enc`。`.env` ファイルは自動読み込みしません。新しいバージョンはCargo.toml/Cargo.lockとCHANGELOGを更新してmainへpushすると、Linux/Windowsのテスト・ビルド成功後にReleaseが作成されます。同名の既存Releaseは上書きしません。

## 参照した公式仕様

- [Discord Voice State / self_stream](https://docs.discord.com/developers/resources/voice)
- [Discord GUILD_CREATEとGatewayイベント](https://docs.discord.com/developers/events/gateway-events)
- [Discord Message Content IntentのHTTP制限](https://docs.discord.com/developers/events/gateway)
- [Mastodon投稿とIdempotency-Key](https://docs.joinmastodon.org/methods/statuses/)
- [Mastodon非同期メディアアップロード](https://docs.joinmastodon.org/methods/media/)
- [GitHub Actionsの料金](https://docs.github.com/en/billing/concepts/product-billing/github-actions)
- [定期実行の遅延](https://docs.github.com/en/actions/how-tos/troubleshoot-workflows) / [60日間更新がない場合の停止](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/disable-and-enable-workflows)

ライセンス: MIT
