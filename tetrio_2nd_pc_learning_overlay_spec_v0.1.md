# Perfect Clear 2nd PC 学習支援オーバーレイ 仕様書

**Version:** Draft v0.1  
**Status:** 要求定義・基本設計ドラフト  
**Primary target:** Windows版 TETR.IO Desktop / ZEN  
**Reference method source:** `Algebruh's 2nd Methods.md`

---

## 1. 目的

本ソフトは、TETR.IO の ZEN 上で Perfect Clear（PC）を練習する際に、2nd PC の既知セットアップをリアルタイムに推薦・表示する学習支援オーバーレイである。

主目的は次のとおり。

- 1st PC 終了後の HOLD / Current / NEXT キューを追跡し、Algebruh の 2nd PC 資料に記載された適合セットアップを自動選択する。
- 選択したセットアップを TETR.IO の盤面上に半透明で重ねて表示する。
- 必要に応じて solve まで段階的に案内する。
- ZEN の Undo を用いた反復練習に対応する。
- 既知のセットアップ・solve の学習を支援し、未知局面から新しい解法を探索するソルバにはしない。

---

## 2. 初版の対象範囲

### 2.1 対象

- OS: Windows
- クライアント: TETR.IO Desktop
- モード: ZEN
- NEXT 表示数: 5
- HOLD: 使用・認識対象
- TETR.IO 標準ミノ色
- 標準 10 列盤面
- ゴーストピース: 有効前提
- UI倍率・ウィンドウサイズ: 可変
- 2nd PC のセットアップ推薦
- PC ordinal（1st〜7th）の追跡
- 7th PC 後の 1st PC への循環
- ZEN Undo による過去状態へのロールバック
- 同一キュー・同一セットアップ上での複数 solve 練習

### 2.2 初版で扱わないもの

- ブラウザ版 TETR.IO
- 対戦モード
- カスタムスキン
- Odd JL, no T の 2nd PC
- 任意盤面からの PC 解探索
- 未登録セットアップの生成
- SRS+ の操作経路探索
- キー入力列の提示
- solve の左右対称性の自動推論
- 3rd 以降のセットアップ推薦
- オンライン通信・TETR.IO内部APIへの依存

---

## 3. 設計上の基本方針

### 3.1 画面認識ベース

TETR.IO の内部状態を直接取得せず、画面から以下を認識する。

- 盤面
- HOLD
- Current
- NEXT 5
- 盤面空化
- 安定状態

画像認識が不確実なフレームではゲーム状態を更新しない。

### 3.2 セットアップ知識は資料由来

推薦対象は `Algebruh's 2nd Methods.md` に登録された既知セットアップ・solve に限定する。

資料の自然言語条件を、起動時または事前変換時に機械可読なルールデータへ落とし込む。

### 3.3 解探索はしない

本ソフトは「現在盤面から PC を見つけるソルバ」ではない。

許可されるフォールバックは次のみ。

1. 現在推薦中のセットアップを継続
2. 既知セットアップとの一致・互換性を再照合
3. 一致する別の登録済みセットアップへ乗り換え
4. 該当なしなら推薦停止

---

## 4. PC セッションモデル

### 4.1 PC ordinal

内部状態として次を保持する。

```text
pc_ordinal ∈ {1, 2, 3, 4, 5, 6, 7}
```

PC 完成により盤面が空になったとき、

```text
1 → 2 → 3 → 4 → 5 → 6 → 7 → 1 → ...
```

と循環する。

2nd PC 推薦機能は `pc_ordinal == 2` の区間で有効になる。

### 4.2 初期同期

通常は「新しい ZEN 試行開始 = 1st PC 開始」として同期する。

途中起動・認識ズレに備え、ユーザーは手動で現在の PC ordinal を 1st〜7th のいずれかへ再同期できる。

### 4.3 1st PC 完了時の 2nd 開始

1st PC 完成による盤面空化が確定した瞬間を、2nd PC の追跡開始点とする。

その時点から、

```text
HOLD + Current + NEXT[0..4]
```

を継続追跡する。

2nd PC ケースの 4 ミノ構成は HOLD を含めて判定する。

例:

```text
HOLD = I
Current = J
NEXT = L, O, ...
```

なら、適切な時点で `IJLO` 系ケースとして分類できる。

---

## 5. Undo 対応

### 5.1 Undo の検出

Undo キーそのものは監視しない。

安定状態ごとに状態スナップショットを保存し、現在状態が過去状態と一致した場合にロールバックとみなす。

概念的な状態キー:

```text
StateKey = {
  board,
  hold,
  current,
  next5,
  pc_ordinal
}
```

必要に応じて認識補助用メタデータを追加する。

### 5.2 Undo 時のロールバック

過去状態へ戻った場合、次も同時に巻き戻す。

- PC ordinal
- 2nd PC ケース分類
- 選択中セットアップ
- キュー履歴
- 推薦候補
- solve 候補
- solve 巡回位置
- 途中まで消化したセットアップセル
- 認識上の分岐状態

### 5.3 solve の反復練習

solve 表示が ON の場合、同じセットアップ開始状態へ Undo で戻ったことを検出したら、適合する別 solve を次候補として提示できる。

例:

```text
solve A
  ↓ Undo
solve B
  ↓ Undo
solve C
```

全候補を一巡した後の挙動は循環を基本とする。

solve 表示が OFF の場合は solve 巡回を視覚的には行わず、セットアップのみ提示する。

---

## 6. 画面認識

### 6.1 認識対象領域

TETR.IO ウィンドウ内から次を検出する。

- 10列盤面
- HOLD
- Current
- NEXT 5
- PC 成功後の空盤面
- 盤面の安定状態

### 6.2 ミノ分類

初版では標準色を利用し、

```text
I, J, L, O, S, T, Z
```

の 7 クラスへ分類する。

色だけでなく、可能であれば形状・セル配置も併用する。

### 6.3 ゴースト

標準ゴースト表示は有効前提。

ゴーストセルは固定盤面セルとして数えない。

Current の識別は操作中実体と UI 情報を用い、ゴーストだけを Current の配置確定として扱わない。

### 6.4 安定状態

ライン消去・アニメーション・ミノ固定直後の一時状態では推薦状態を更新しない。

以下が一定時間または一定フレーム連続で成立したときのみ「安定状態」とする。

- 盤面セル配置が一致
- HOLD が一致
- Current が一致
- NEXT 5 が一致
- 認識信頼度が閾値以上

具体的なフレーム数・時間閾値は実装時に調整可能なパラメータとする。

### 6.5 認識不能時

次のいずれかが起きた場合、内部ゲーム状態を更新しない。

- TETR.IO ウィンドウが見つからない
- 盤面位置を確定できない
- HOLD / Current / NEXT の一部を認識できない
- 設定画面・メニュー等で通常 UI が崩れている
- 信頼度が閾値を下回る

復帰後、現在状態と履歴を照合して追跡を再開する。

---

## 7. 2nd PC ケース分類

資料の分類体系を維持する。

上位分類は少なくとも以下。

```text
- Even JL, no T
- Even JL, with T
- Odd JL, with T
- Other Necessary Dupes
- Odd JL, no T   # 初版対象外
```

個別ケース例:

```text
IJLO
IJLS / IJLZ
JLOS / JLOZ
IOSZ
TIJL
TISZ
TIJO / TILO
...
```

左右対称ケースは内部正規化可能だが、solve の成立性までは対称と仮定しない。

---

## 8. ミラー処理

### 8.1 正規化に使えるもの

ケース分類・盤面形状の正規化では次を利用できる。

```text
J ↔ L
S ↔ Z
x ↔ 9 - x
```

### 8.2 自動ミラーしてよいもの

- ケース名
- セットアップ形状
- 条件中の J/L, S/Z
- 座標
- 表示用セル配置

### 8.3 自動ミラーしてはいけないもの

TETR.IO SRS+ では到達経路・キック等が左右対称とは限らない。

したがって、以下は資料に明示されたデータのみを使用する。

- solve の成立
- スピン
- 180°
- skim
- 特殊な配置順
- 操作経路依存の成立条件

本ソフトは SRS+ の経路探索を行わない。

---

## 9. 推薦優先順位

候補セットアップが複数ある場合、次の順で決定する。

### 9.1 方法カテゴリ優先度

```text
1. Save O
2. alt
3. T/I
4. extra
```

ただし、資料内で同一 method 内のセットアップ優先順が明記されている場合は、その局所的優先順を保持する。

### 9.2 同順位タイブレーク

同じ優先カテゴリなら、

```text
1. より早く条件が確定する
2. 必要セットアップピース数が少ない
3. 資料掲載順が早い
```

の順で選ぶ。

### 9.3 候補の再評価

NEXT が新たに公開された場合、現在のキュー履歴を用いて候補条件を再評価する。

一度選んだ候補も、資料上の fail 条件が確定した場合は除外する。

---

## 10. キュー追跡

### 10.1 基本

NEXT の「最初の n 個」等は、1st PC が終わった瞬間から観測した順序を基準にする。

画面に現在見えている 5 個だけを毎回独立評価するのではなく、公開履歴を保持する。

概念:

```text
observed_queue_history = [
  piece_0,
  piece_1,
  piece_2,
  ...
]
```

HOLD / Current の変化と NEXT のシフトから、新たに公開されたミノを追加する。

### 10.2 条件表現

資料中の自然言語条件を、たとえば以下へ変換する。

```text
see S
S before J
Z before L
S before any two of [O, Z, T]
I/T last
S last
[TJ]!,S
[TOI]!
hold T
access O
see another L in first 3
```

内部では論理述語として保持する。

---

## 11. ルールデータモデル

推奨形式は YAML / JSON 相当の構造化データとする。

例:

```yaml
case:
  id: IJLO
  family: even_jl_no_t

methods:
  - id: ijlo_save_o
    source_order: 1
    category: save_o

    setups:
      - id: ijlo_s_qb
        setup_piece_count: 1

        build_condition:
          all:
            - seen: S
            - order: [S, "<", J]

        fail_condition:
          any:
            - queue_pattern: "[TJ]!,S"

        save:
          piece: O

        placements:
          canonical:
            - piece: S
              cells: [...]

        solves:
          - id: solve_1
            condition:
              order: [S, "<", Z]
            target_cells: [...]

          - id: solve_2
            condition:
              is_last: S
            target_cells: [...]
```

実際の IJLO データは資料記載内容を精査して別途入力する。

---

## 12. 条件 DSL

複雑な自然言語条件を一貫して扱うため、ルールエンジンでは小さな条件 DSL を定義する。

候補:

```text
seen(piece)
not_seen(piece)
before(a, b)
after(a, b)
last(piece, set_or_window)
in_first_n(piece, n)
before_any(piece, set, count)
queue_matches(pattern)
hold_is(piece)
current_is(piece)
accessible(piece)
all(...)
any(...)
not(...)
```

資料中の表現を無理に一般化しすぎず、必要になった条件だけを追加する。

---

## 13. セットアップ一致判定

### 13.1 通常

推薦中セットアップについて、配置済みセルが想定形状と整合しているか確認する。

### 13.2 逸脱

ライン消去処理まで終わった安定状態で、現在盤面が推薦セットアップと整合しなくなった場合:

1. 現在の既知候補を再評価
2. 現在盤面が別の登録済みセットアップの途中状態と一致するか検索
3. 一致すればそのセットアップへ乗り換え
4. 一致しなければ推薦停止

未知盤面を補完する探索はしない。

---

## 14. オーバーレイ表示

### 14.1 ウィンドウ

- 透明
- 最前面
- クリック透過
- TETR.IO ウィンドウ位置・サイズへ追従

### 14.2 セットアップ表示

選択したセットアップの完成予定セルを盤面へ半透明で表示する。

- 未配置セル: 薄い表示
- Current の推奨配置: 強調表示
- 既に正しく配置済みのセル: オーバーレイから消す
- 不一致セル: 必要に応じて警告表示

### 14.3 solve 表示

設定:

```text
show_solve = ON | OFF
```

ON:
- setup 完成後または solve 条件確定後に solve 形状を表示
- Undo 後の別 solve 巡回を有効化

OFF:
- setup のみ表示
- solve はプレイヤー自身で解く
- 内部条件判定は保持してよいが、solve 形状は描画しない

---

## 15. ホットキー

初版で必要なホットキー:

```text
- オーバーレイ ON/OFF
- solve 表示 ON/OFF
- PC ordinal 手動再同期
```

再同期 UI は、ホットキーで小さな選択 UI を開き、

```text
1st / 2nd / 3rd / 4th / 5th / 6th / 7th
```

から選ぶ方式を基本とする。

必要であれば将来追加:

```text
- 次の solve
- 前の solve
- 現在セットアップの固定
- 再認識
- デバッグ表示
```

---

## 16. 状態機械

概略:

```text
[NO_WINDOW]
    ↓ TETR.IO検出
[SYNC_WAIT]
    ↓ ZEN開始 / 手動同期
[TRACK_PC]
    ↓ pc_ordinal == 2
[CLASSIFY_2ND]
    ↓ ケース判定
[SELECT_SETUP]
    ↓ 推薦
[GUIDE_SETUP]
    ├─ 新NEXT公開 → 条件再評価
    ├─ Undo → 過去状態へロールバック
    ├─ 逸脱 → REGISTERED_FALLBACK
    └─ setup完成 → GUIDE_SOLVE or SELF_SOLVE
                           ↓
                     PC完成
                           ↓
                       TRACK_PC
```

認識不能時は状態機械を進めず、最後の確定状態を保持する。

---

## 17. 状態スナップショット

推奨データ:

```text
Snapshot {
    timestamp
    pc_ordinal

    board[20][10]
    hold
    current
    next5

    observed_queue_history

    case_id
    method_id
    setup_id
    setup_progress

    solve_candidates[]
    solve_index

    recognition_confidence
}
```

盤面高さは内部的に TETR.IO の可視・非可視領域との対応を踏まえて調整可能とする。

---

## 18. 資料変換パイプライン

`Algebruh's 2nd Methods.md` は人間向け資料であり、画像と自然言語条件が混在する。

初版では次の変換工程を想定する。

```text
Markdown
  ↓
case / method セクション分割
  ↓
setup画像・solve画像抽出
  ↓
自然言語条件を手動または半自動で構造化
  ↓
左右対称ケースの正規化
  ↓
ルールデータ
  ↓
バリデーション
  ↓
アプリから読込
```

完全自動変換は目標としない。

資料内の微妙な queue 条件・例外・SRS+依存事項は、人間によるレビューを必須とする。

---

## 19. IJLO を用いた構造化方針例

IJLO はスキーマ検証用の最初のケースとして用いる。

資料では IJLO に対して複数の queue-based setup と fail queue、fallback、solve の順序条件が記載されているため、以下を一通り検証できる。

- `seen`
- `before`
- fail queue
- setup priority
- Save O / Save T
- fallback
- solve 分岐
- sandbox 系セットアップ
- HOLD を含む構築可能性
- NEXT 公開による条件確定

まず IJLO を完全に構造化してスキーマを固定し、その後ほかのケースへ展開する。

---

## 20. エラー・異常系

### 20.1 低信頼度

画面認識信頼度が低い場合:

- 推薦更新停止
- 最後の確定状態を保持
- 必要に応じて小さな認識警告を表示

### 20.2 履歴不一致

現在状態が通常遷移にも既知 Undo 履歴にも一致しない場合:

- 安定フレームを再確認
- 一定回数失敗後に「再同期推奨」
- PC ordinal を勝手に推測し直さない

### 20.3 未対応ケース

Odd JL, no T など初版未対応ケース:

- 誤った推薦を行わない
- 「未対応ケース」と表示
- 追跡自体は継続可能

### 20.4 既知セットアップなし

資料ルール上候補が存在しない、または現在盤面が全候補から逸脱した場合:

- オーバーレイ推薦停止
- 解探索はしない
- 必要に応じて「登録済み候補なし」と表示

---

## 21. 非機能要件

### 21.1 遅延

プレイを妨げない範囲で低遅延に状態更新する。

目標値は実装検証後に確定する。

### 21.2 負荷

ZEN プレイ中に TETR.IO の描画・入力へ目立つ影響を与えない。

毎フレーム重い全画面認識を行うのではなく、ウィンドウ位置が安定している間は既知 ROI を使う。

### 21.3 誤案内より停止を優先

認識不確実時は、誤ったセットアップを表示するより推薦停止を優先する。

### 21.4 データ分離

セットアップルール・画像データはアプリ本体コードから分離し、資料更新や追加ケースに対応しやすくする。

---

## 22. デバッグ・検証支援

開発用モードでは以下を表示可能にする。

- 認識した HOLD / Current / NEXT
- queue history
- pc_ordinal
- case_id
- 候補セットアップ一覧
- 候補除外理由
- 選択理由
- fail condition
- 現在の StateKey / 履歴一致
- 認識信頼度
- 盤面セル認識結果

推薦ミスの原因を「画面認識」「ルールデータ」「優先順位」「Undo同期」に切り分けられることを重視する。

---

## 23. 将来拡張候補

- Odd JL, no T 対応
- 3rd〜7th PC の学習支援
- カスタムスキン認識
- ブラウザ版
- 40L / Practice 等の他モード
- セットアップ検索 UI
- 苦手ケース統計
- solve 正答率・反応時間の記録
- spaced repetition 的な練習キュー
- ユーザー独自 method データの追加
- セットアップ画像からの半自動データ入力
- 複数資料の method 切替

---

## 24. 現時点の未確定事項

要求仕様の主要部分は確定済み。

今後、実装前に詳細化すべき項目:

1. TETR.IO UI 各 ROI の検出方法
2. 安定フレーム判定の具体的閾値
3. ミノ分類器の方式
4. ゴーストと固定セルの色・輝度判定
5. board の高さ・非可視領域の扱い
6. Undo 状態照合に使う StateKey の厳密な構成
7. solve 巡回の細かい UX
8. ホットキー初期値
9. ルール DSL の最終仕様
10. Algebruh 資料全体の構造化・レビュー
11. セットアップ画像をセル座標へ変換する作業手順
12. SRS+ 非対称 solve をどの単位で明示登録するか

---

## 25. 実装前ロードマップ

### Phase A — スキーマ確定

1. IJLO を完全に構造化
2. 条件 DSL を不足なく定義
3. ミラー表現を検証
4. Save O / alt / T/I / extra 優先順位をテスト
5. solve 表示 ON/OFF をデータモデル上で検証

### Phase B — 認識 PoC

1. TETR.IO ウィンドウ取得
2. ZEN の盤面 ROI 検出
3. HOLD / Current / NEXT5 認識
4. 固定盤面 / ゴースト分離
5. 空盤面検出
6. 安定状態検出

### Phase C — 状態追跡

1. queue history
2. PC ordinal
3. 1st→2nd 境界
4. 7th→1st 循環
5. Undo ロールバック
6. 手動再同期

### Phase D — 推薦

1. IJLO のみで end-to-end 動作
2. setup オーバーレイ
3. solve オーバーレイ
4. 既知セットアップへのフォールバック
5. solve Undo 巡回

### Phase E — 資料全体展開

1. 対象 2nd ケースを順次構造化
2. 自動バリデーション
3. 実プレイテスト
4. 認識・ルール双方の回帰テスト

---

## 26. 成功条件

初版は、少なくとも以下を満たしたとき完成とみなす。

- Windows版 TETR.IO Desktop / ZEN で安定して起動できる。
- 1st PC 完成を検出し、2nd PC を正しく開始できる。
- HOLD / Current / NEXT5 を追跡できる。
- 登録済みケースを正しく分類できる。
- 資料の条件に従って適切なセットアップを選べる。
- セットアップを盤面上へ遅延少なく表示できる。
- solve 表示をホットキーで切り替えられる。
- Undo で内部状態が正しく巻き戻る。
- Undo 後に別 solve を反復練習できる。
- 7th PC 後に再び 1st PC として追跡できる。
- 未対応・不確実状態で誤推薦せず停止できる。
- 未知局面からの解探索を行わない。

