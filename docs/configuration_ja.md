# 設定

keifu は `~/.config/keifu/config.toml` で設定できます。すべての設定は任意です。

## 自動更新

デフォルトでは、keifu は 10 秒ごとにコミットグラフを更新し、60 秒ごとに origin から fetch します。

```toml
[refresh]
# ローカル状態の自動更新を有効にする（デフォルト: true）
auto_refresh = true

# ローカル更新の間隔（秒）（デフォルト: 10、最小: 1）
refresh_interval = 10

# origin からの自動 fetch を有効にする（デフォルト: true）
auto_fetch = true

# リモート fetch の間隔（秒）（デフォルト: 60、最小: 10）
fetch_interval = 60
```

### オプション一覧

| キー | 型 | デフォルト | 説明 |
| --- | --- | --- | --- |
| `auto_refresh` | bool | `true` | ローカル状態（コミット、ブランチ、ワーキングツリー）の自動更新を有効にする |
| `refresh_interval` | integer | `10` | ローカル更新の間隔（秒）（最小: 1） |
| `auto_fetch` | bool | `true` | origin からの自動 fetch を有効にする |
| `fetch_interval` | integer | `60` | リモート fetch の間隔（秒）（最小: 10） |

### 自動更新を無効にする

自動更新を完全に無効にするには:

```toml
[refresh]
auto_refresh = false
auto_fetch = false
```

手動での更新は `R` キー、fetch は `f` キーで引き続き可能です。

## キーボードショートカット

`[keymap]` テーブルでアクションごとのショートカットを置き換えられます。
未指定のアクションは従来のデフォルトを維持します。各アクションには複数の
代替キーを指定でき、空の配列は明示的な割り当て解除を表します。

```toml
[keymap]
pull = ["Ctrl+Alt+P"]
open-command-palette = ["Ctrl+P", "Ctrl+Alt+P", "F2"]
toggle-debug-keys = []
```

アクション名はコマンドレジストリと共有される安定した kebab-case 識別子です。
例: `fetch`、`pull`、`push`、`refresh`、`open-commit-menu`、
`open-command-palette`、`open-settings`、`toggle-help`、`move-up`、
`move-down`、`menu-select`、`confirm`、`cancel`、`toggle-stage`。

`Ctrl`、`Alt`、`Shift` とその組み合わせ、矢印や `Enter`、`Esc`、
`PageUp`、`PageDown`、および `F1`〜`F24` を利用できます。1 つの設定は
単一キーのみで、`Ctrl+K Ctrl+C` のような複数キーのコードは未対応です。

不正なキーや未知のアクションは、その項目だけが拒否され、対象アクションは
デフォルトを維持します。他の有効な設定は適用され、起動時のエラートーストと
ログに理由が表示されます。同時に有効なコンテキストで競合した場合は TOML の
後の項目が優先され、両方のアクション名を含む警告が表示されます。互いに排他的な
コンテキストでは同じキーを再利用できます。ヘルプとコマンドパレットには、複数
候補や `Unassigned` を含む実際の割り当てが表示されます。
