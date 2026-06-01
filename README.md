# spotify_lyrics

Spotify の再生状況に追従して歌詞を表示するデスクトップアプリです。  
`LRCLIB` と `Spotify official` の 2 つの取得元を切り替えできます。

## インストール

GitHub Releases の最新の `.deb` をダウンロードして、以下でインストールできます。

```bash
sudo apt install ./spotify_lyrics_*.deb
```

同じコマンドで更新できます。既存の古い `deb` を消して入れ直す必要はありません。

## 起動

インストール後は通常どおりアプリを起動してください。

## 使い方

### 歌詞取得元を切り替える

設定画面の `Lyrics source` で `LRCLIB` か `Spotify official` を選べます。

### Spotify official を使うための `sp_dc`

`Spotify official` を使うには、Spotify のブラウザ cookie である `sp_dc` が必要です。  
これは `librelyrics-spotify` の README にある手順と同じです。

手順:

1. ブラウザで [Spotify Web Player](https://open.spotify.com) を開く
2. Spotify にログインする
3. 開発者ツールを開く
4. `Application` タブの `Cookies` から `sp_dc` を探す
5. `sp_dc` の値をコピーして、アプリの設定画面の `Spotify sp_dc` に貼り付ける

### 常に最前面にする

アプリを起動したら、ウィンドウ上部を右クリックして `常に最前面に表示` を選ぶと、前面固定にできます。

## 補足

- `Spotify official` は Spotify の内部 API を使って歌詞を取得します。
- `LRCLIB` は通常の歌詞取得用です。
- 文字単位の追従はヒューリスティックです。行単位のタイムスタンプを元に塗り分けています。
