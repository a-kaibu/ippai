; 吉里吉里ベンチ: ippai のベンチマーク (tests/bench, グループ kirikiri) が遊ぶ小さなノベル。
; tests/bench/scenario.mjs がプレイヤーとしてクリックして進める。合図は Debug.message の行
; (krkrz は標準出力に出す):
;   BENCH-STATE <何> <番号>   何かを表示し終えた (title, scene, line, saved, restored, end)
;   BENCH-WAIT <番号> <何>    クリックを待ち始めた (click, idle: 3 秒待ってから, choice: 選択肢)
; 番号は両方で数える。-mode=load で起動するとセーブを読んで restored で止まる。
; fx_* はクリックから重い演出の終わりまで (特殊トランジション、移動と揺れ、文字送り、画像処理)。
; 花びらの間は bench.tjs が描いた間隔を "BENCH kk_petals_p95 ms" で出す。

; ---- 合図 ----
[macro name=st][eval exp="Debug.message('BENCH-STATE ' + mp.w + ' ' + (++tf.step))"][endmacro]
[macro name=bw][eval exp="Debug.message('BENCH-WAIT ' + (++tf.step) + ' ' + mp.w)"][endmacro]

; ---- 台詞: [line t=本文 voice=ボイス (face=立ち絵) (idle=1)] ----
[macro name=line]
[cm]
[if exp="mp.face !== void"][image storage=%face layer=0 page=fore visible=true left=440 top=80][endif]
[playse storage=%voice buf=0]
[nowait][emb exp="mp.t"][endnowait]
[st w=line]
[if exp="mp.idle !== void"][bw w=idle][else][bw w=click][endif]
[p]
[endmacro]

; ---- 場面転換: [scene bg=背景 (ch=立ち絵) (rule=ルール画像 | method=extrans の方法 time=ms) (w=合図の名前)] ----
[macro name=scene]
[cm][backlay]
[image storage=%bg layer=base page=back]
[if exp="mp.ch !== void"][image storage=%ch layer=0 page=back visible=true left=440 top=80][else][freeimage layer=0 page=back][endif]
[if exp="mp.method !== void"][trans method=%method time=%time|1000][elsif exp="mp.rule !== void"][trans method=universal rule=%rule vague=64 time=600][else][trans method=crossfade time=500][endif]
[wt]
[st w=%w|scene][bw w=click][waitclick]
[endmacro]

[loadplugin module=extrans.dll]
[eval exp="Scripts.execStorage('bench.tjs')"]
[eval exp="tf.step = 0"]
[position layer=message0 page=fore left=40 top=520 width=1200 height=180 marginl=24 margint=20 opacity=160]
[position layer=message0 page=back left=40 top=520 width=1200 height=180 marginl=24 margint=20 opacity=160]
[if exp="System.getArgument('-mode') == 'load'"][eval exp="tf.loaded = true"][load place=0][s][endif]

*title|タイトル
[image storage=title layer=base page=fore]
[nowait]吉里吉里ベンチ　―　クリックではじめる[endnowait]
[st w=title][bw w=click][waitclick]
[playbgm storage=bgm0 loop=true]

; ---- 一: 丘 ----
[scene bg=bg1 ch=ch0_0]
[line voice=voice0 t="「おはよう。今日もいい天気だね」"]
[line voice=voice1 face=ch0_1 t="「丘の上まで行ってみない？　向こうに町が見えるはず」"]
[line voice=voice2 idle=1 t="風が草をなでていく。遠くで鳥が鳴いた。"]
[line voice=voice3 face=ch0_2 t="「ほら、急がないと日が高くなっちゃう」"]
[line voice=voice4 t="そう言って、彼女は先に歩き出した。"]

; 移動と揺れ: 立ち絵を横から滑らせて画面を揺らす (move は今の位置から三区間 x 700 ms = 2.1 秒が本来の長さ)
[cm]
[move layer=0 path="(-200,80,0)(140,80,255)(440,80,255)" time=700 accel=-2]
[quake time=500 hmax=16 vmax=16]
[wm][wq]
[st w=fx_move][bw w=click][waitclick]

; 三人並べて一度に表情を変える
[cm][backlay]
[image storage=ch0_1 layer=0 page=back visible=true left=40 top=80]
[image storage=ch1_1 layer=1 page=back visible=true left=440 top=80]
[image storage=ch2_1 layer=2 page=back visible=true left=840 top=80]
[trans method=crossfade time=400][wt]
[st w=fx_three][bw w=click][waitclick]
[cm][backlay]
[freeimage layer=1 page=back][freeimage layer=2 page=back]
[trans method=crossfade time=300][wt]

; ---- 二: 夕方の道 (ルール画像で) ----
[scene bg=bg2 ch=ch1_0 rule=wipe]
[eval exp="benchPetals(48)"]
[line voice=voice5 t="「やっと追いついた。足、速すぎ」"]
[line voice=voice6 face=ch1_1 t="「そっちが遅いだけだよ」"]
[line voice=voice7 t="いつのまにか空の色が変わり始めていた。"]
[line voice=voice8 face=ch1_2 t="「この先で道がふたつに分かれてるんだ」"]

; 文字送り: 縁取りとルビつきの長い文を一文字ずつ (花びらは降ったまま)
[cm][delay speed=10][font edge=true edgecolor=0x402030]
花びらが[ruby text=ま]舞[ruby text=ちゅう]宙をただよい、夕日を受けてきらきらと光っていた。ふたりはしばらく黙ったまま、その景色を眺めていた。風が吹くたびに、[ruby text=はなびら]花弁はくるくると回りながら道の向こうへ流れていく。
[resetfont][delay speed=nowait]
[st w=fx_typed][bw w=click][p]
[eval exp="benchPetalsStop()"]

; 回想: ぼかしてセピアに
[cm]
[eval exp="benchBlur()"]
[st w=fx_blur][bw w=click][waitclick]
[scene bg=bg2 ch=ch1_1 method=ripple time=1200 w=fx_ripple]

; ---- 三: 分かれ道 ----
[scene bg=bg3 ch=ch2_0 method=wave time=1000 w=fx_wave]
[line voice=voice9 t="「左は森、右は川。どっちにする？」"]
[line voice=voice10 face=ch2_1 t="「決めるのはきみだよ」"]
[line voice=voice11 t="少し考えてから、ぼくは答えた。"]
[line voice=voice12 face=ch2_2 t="「……うん、そっちにしよう」"]

; ---- 選択肢: 画面いっぱいの半透明のメッセージレイヤ (透明だとクリックが下へ抜ける) にリンクふたつ (ひとつめがハーネスのクリック 640,430 の下) ----
[xchgbgm storage=bgm1 loop=true time=1500]
[cm]
[current layer=message1 page=fore]
[position layer=message1 page=fore left=0 top=0 width=1280 height=720 frame="" color=0x000000 opacity=96 visible=true]
[font size=48]
[locate x=400 y=400][link target=*left]　左の道を行く　[endlink]
[locate x=400 y=520][link target=*right]　右の道を行く　[endlink]
[resetfont]
[st w=fx_choice][bw w=choice][s]

*left
[er][layopt layer=message1 visible=false][current layer=message0]
[scene bg=bg4 ch=ch0_0 method=rotatezoom time=1000 w=fx_rotatezoom]
[jump target=*after]

*right
[er][layopt layer=message1 visible=false][current layer=message0]
[scene bg=bg5 ch=ch1_0 method=rotatezoom time=1000 w=fx_rotatezoom]

*after
[line voice=voice13 t="木々のあいだから、やわらかい光がこぼれていた。"]
[line voice=voice14 face=ch0_1 t="「ここ、覚えておこう。また来たいから」"]
[line voice=voice15 t="ぼくはその景色を栞にはさむことにした。"]

; ---- セーブ: KAG は最後のラベルで保存し、ロードするとそこから始める ----
*saved|保存した場面
[if exp="tf.loaded"][nowait]（ロードしました）[endnowait][st w=restored][eval exp="Debug.message('BENCH-END')"][s][endif]
[save place=0]
[st w=saved][bw w=click][p]

; ---- 四: 町 ----
[scene bg=bg6 ch=ch2_0 method=mosaic time=1000 w=fx_mosaic]
[line voice=voice16 t="丘を下りきると、町の灯りがともり始めていた。"]
[line voice=voice17 face=ch2_1 t="「着いたね。おなか空いた」"]
[line voice=voice18 t="ふたりで笑って、灯りのほうへ歩いていった。"]
[line voice=voice19 face=ch2_2 t="「また明日も、どこか行こう」"]

[stopbgm]
[cm][nowait]おしまい[endnowait]
[st w=end][eval exp="Debug.message('BENCH-END')"][s]
