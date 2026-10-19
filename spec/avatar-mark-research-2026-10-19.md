# CTCP AVATAR の目印と IRCnet の調査 2026-10-19

IRCnet で「発言してもアイコンが入れ替わらず、WHOIS して初めて出る」という報告を受けて調べた結果の記録です。決定そのものは `spec/decisions.md` の D025 にあり、ここにはその根拠になった観測と、他の実装の調査を残します。関連 Issue は #409、#488、#490 です。

他のプロジェクトのソースは、挙動を確かめるために読んだだけです。コードは写していません（D042）。

## 1. IRCnet の WHO の返事にはサーバー ID が入る（#488）

### 観測
irc6.ircnet.ne.jp:6667 に `cayen_bot` で接続し、`WHO <nick>` の返事（352）をそのまま記録した。

```
:dh.ircnet.ne.jp 352 cayen_obs #CayenChat ~cayen_bot <host> dh.ircnet.ne.jp cayen_bot H :0 392L <realname>
```

- 最後の引数が `<hop 数> <サーバー ID> <実名>` になっている。RFC 2812 の `<hopcount> <real name>` とは違う。
- 登録時に 042（RPL_YOURID）で自分の UID（例：`392LAGUR7`）が届き、その先頭 4 文字がサーバー ID になっている。
- WHOIS（311）の実名にはサーバー ID が入らない。

### ircd の実装（ircnet/ircd e6d60c4）
- `s_err.c` の 352 の書式は、hop 数、サーバー ID、実名の順。
- `s_user.c` の `who_one` が、相手のサーバーの `sid` を入れている。
- `s_id.c` の `sid_valid` によると、サーバー ID は「数字 1 文字と `[0-9A-Z]` 3 文字」の 4 文字（SIDLEN）。

### 不具合の原因
CayenChat は 352 の最後の引数を最初の空白で分け、残りを実名として読んでいた。IRCnet では `392L <実名>` を実名として読むため、先頭の目印が見つからない。その相手は「目印なし」と記録され、問い合わせ直さない。一方、WHOIS（311）にはサーバー ID がないので目印が見つかる。発言では出ず、WHOIS では出たのはこのため。

### 他のクライアント
KVIrc（`parseNumericWhoReply`）、irssi（`event_who`）、WeeChat、HexChat、Quassel、Konversation を見た。どれも hop 数の後ろをすべて実名として扱い、サーバー ID を特別に扱うものはなかった。

### 対応
#488：042 を受け取った接続でだけ、352 の hop 数の後ろにあるサーバー ID の形の語を飛ばす。RFC どおりのサーバーで、実名がたまたまその形の語で始まる場合を読み違えないようにするため。

## 2. ISO-2022-JP で KVIrc の目印を生のバイトで送れるか（#490）

KVIrc の目印は、実名の先頭の `0x03`（ETX）、4 のビットが立った数字、`0x0F`（SI）の 3 バイト。D025 では、ISO-2022-JP の接続ではこの目印を付けないことにしている。理由は、`encoding_rs` が SI を送信時に `&#65533;`、受信時に U+FFFD にしてしまうため。そこで、変換を通さずに生のバイトで送れないかを調べた。

### IRCnet は通す
irc6.ircnet.ne.jp（dh.ircnet.ne.jp）に、実名の先頭に `0x03 '4' 0x0F` を生のバイトで付けて接続し、#CayenChat に入った。

```
352 ... cayen_bot H :0 392L \x034\x0fcayen_bot raw mark test
311 ... cayen_bot ... * :\x034\x0fcayen_bot raw mark test
```

自分への WHOIS でも、他の接続からの WHO と WHOIS でも、3 バイトがそのまま返った。サーバーは SI を削らず、断りもしない。

### KVIrc は変換した後の位置を決め打ちで見る
`src/kvirc/sparser/KviIrcServerParser_numericHandlers.cpp`（kvirc/KVIrc master）の WHO の返事の処理では、接続の文字コードで変換した後の実名について、次をすべて満たすときに AVATAR を問い合わせる。

- 1 文字目が U+0003
- 2 文字目の文字コードで 4 のビットが立っている（数字でなくてもよい）
- 3 文字目が U+000F（`KviControlCodes::Reset`）

位置が決まっているので、前や間に別のバイトを足して回避することはできない。また、ISO-2022-JP で U+000F を作れるのは生の 0x0F だけで、エスケープシーケンスで別に書く方法はない。

### LimeChat（Mac 版）が不安定になる
生のバイトの目印を付けて接続している間、メンテナーの LimeChat が不安定になった。ソース（psychs/limechat ceee3d1）での受信の流れは次のとおり。

- `IRCClient.m` の `ircConnectionDidReceive:` では、ISO-2022-JP のとき、受け取った行を `NSData+Kana.m` の `convertKanaFromNativeToISO2022` に通す。この前処理は、`ESC ( J` または `ESC ( I` の後の状態でしか SO と SI を見ない。行の最初の状態では 0x0E と 0x0F をそのまま渡す。
- その結果を macOS 標準の `NSString initWithData:encoding:NSISO2022JPStringEncoding` で文字列にする。
- 失敗して nil が返ると、行の末尾を 1 バイトずつ削りながら、成功するまで変換をやり直す（「途切れたエスケープシーケンスを避ける」ための処理）。それでも失敗すれば ASCII として読む。

macOS の変換が 0x0F を受け付けないとすると、目印を含む 352 と 311 は、0x0F より後ろが削られた行として扱われる。さらに、その行のたびに変換のやり直しが走る。不安定になったのはこのためとみられる。macOS の変換が 0x0F を拒むことは、ソースからは確かめていない。

### SO を組み合わせる案
`03 0E 0F`（ETX、SO、SI）は、形の上では KVIrc の条件に合う。SO（0x0E、`00001110`）は 4 のビットが立っているので、2 文字目の条件を満たすから。また、SO/SI で半角カタカナを切り替える方式（CP50222 など）として見れば、「切り替えてすぐ戻る」という、つじつまの合った並びにもなる。しかし、次の理由で採らない。

- 標準の ISO-2022-JP（RFC 1468）では、SO も SI と同じく使ってはいけない。LimeChat の前処理は 0x0E もそのまま macOS の変換に渡すので、同じように失敗するとみられる。
- 変換器が SO/SI 方式に対応していると、SO と SI は切り替えの制御として消え、変換した後の文字列に残らない。そうなると KVIrc の判定も通らない。

### 結論
- ISO-2022-JP の接続では、KVIrc が自動で見つけられる目印は付けられない。KVIrc に通じる唯一のバイトが、LimeChat を不安定にするバイトでもあるため。
- D025 のとおり、ISO-2022-JP では KVIrc の目印を付けない。CayenChat 同士は文字の目印 `\x034\x03` で見つけ合う。
- #490 では、文字の目印を付けるスイッチを足す。既定はオフ。
- KVIrc の利用者から AVATAR を問い合わせられたら、今までどおり答える。

## 3. bouncer と実名（#490）

bouncer は、サーバーへの `USER` に自分の設定の実名を使う。そのため、クライアントの実名の設定は、bouncer 経由ではサーバーに届かない。

- **Tiarra**（topia/tiarra d562727）
  - 実名は `main/IrcIO/Server.pm` の `config_local_or_general('name')` で決まる。ネットワークのブロックの `name` が優先され、なければ `general` の `name` が使われる。
  - WHO と WHOIS は、どちらも `_WHOIS_from_client` の経路でサーバーに渡される。チャンネル名あての WHO だけは `Client::Cache` が答えることがある。
  - 複数のサーバーにつなぐ設定では、315（WHO の終わり）の引数に `@ネットワーク名` が付く。CayenChat の問い合わせの終わりと一致しなくなり、その問い合わせは 30 秒の期限まで残る。
- **soju**（3dec1ed）
  - クライアントが `USER` で送った実名は使わない。サーバーへの実名は、ネットワーク、ユーザー、ニックネームの順に決まる（`database.GetRealname`）。
  - `network update <ネットワーク名> -realname <実名>`、またはそのネットワークにつないだ状態でクライアントから `SETNAME` を送ると、ネットワークごとの実名として保存される。
  - サーバーが `setname` に対応していない場合（IRCnet など）は、サーバーとの接続を一度つなぎ直して反映する。
  - クライアントには、いつも `setname` を提供する。

どちらもネットワークごとに実名を設定できる。bouncer 経由で文字の目印を使う人は、目印を付けたいネットワークの実名に手で書く。

## 4. ほかに分かったこと（未対応）
- 発言の行は、アイコンが分かる前に受け取った行なので、既定のアイコンのまま残る（`spec/architecture.md` の "Identity policy"）。問い合わせのきっかけになった発言の行は、後からアイコンが分かっても変わらない。
- WHO の問い合わせが期限切れになった場合や、263（混雑）で断られた場合、その相手には問い合わせ直さない。
