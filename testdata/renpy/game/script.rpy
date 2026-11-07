## Ren'Py ベンチ: tests/bench/scenario.mjs がプレイヤーとしてクリックして進める。合図は標準出力の行
## (WriteFile で直接: Ren'Py の exe はコンソールのない GUI プログラム):
##   BENCH-STATE <何> <番号>   何かを表示し終えた (title, scene, line, saved, restored, end, fx_*)
##   BENCH-WAIT <番号> <何>    クリックを待ち始めた (click, idle: 3 秒待ってから, choice: 選択肢)
## 番号は両方で数える。引数 (ベンチの args) は '"" <方式>': 空の basedir (exe の場所) とコマンド
##   gl / angle            OpenGL (gl2) / ANGLE 経由の Direct3D 11 (angle2) で最初から遊ぶ
##   gl_load / angle_load  セーブを読んで restored で止まる
## fx_* はクリックから重い演出の終わりまで (ImageDissolve、移動と揺れ、三人のディゾルブ、文字送り、
## GLSL のぼかしとセピア、自作シェーダの波紋、ピクセレート、回転ズーム、4 秒の動画)。
## 花びらの間は毎フレーム描き直す displayable が描いた間隔を "BENCH rp_petals_p50/p95 ms" で出す。

init -100 python:
    import sys, os, time, ctypes

    # ---- 合図 ----
    _bench_step = [0]

    def bench_out(s):
        b = (s + "\n").encode("utf-8")
        if not hasattr(ctypes, "windll"):  # (the Linux SDK compiling the scripts)
            return
        k = ctypes.windll.kernel32
        k.GetStdHandle.restype = ctypes.c_void_p
        n = ctypes.c_ulong(0)
        k.WriteFile(ctypes.c_void_p(k.GetStdHandle(-11)), b, len(b), ctypes.byref(n), None)

    def bench_state(w):
        _bench_step[0] += 1
        bench_out("BENCH-STATE %s %d" % (w, _bench_step[0]))

    def bench_wait(w="click"):
        _bench_step[0] += 1
        bench_out("BENCH-WAIT %d %s" % (_bench_step[0], w))

    # ---- 方式: コマンド (sys.argv の最後) で描画を選ぶ。ディスプレイの初期化より前 ----
    _bench_cmd = sys.argv[-1] if len(sys.argv) > 1 else "gl"
    bench_load = _bench_cmd.endswith("_load")
    os.environ["RENPY_RENDERER"] = "angle2" if _bench_cmd.startswith("angle") else "gl2"

    def _bench_command():
        renpy.arguments.takes_no_arguments("Ren'Py bench")
        return True

    for _c in ("gl", "angle", "gl_load", "angle_load"):
        # (uses_display: otherwise Ren'Py starts SDL's dummy video driver and draws in software)
        renpy.arguments.register_command(_c, _bench_command, uses_display=True)

    # ---- 台詞: 表示し終えたら line、クリックを待つ (idle なら 3 秒待たれる) ----
    def bench_line_cb(event, interact=True, **kw):
        if event == "slow_done" and interact:
            bench_state(store._bench_kind[0])
            bench_wait(store._bench_kind[1])

    _bench_kind = ["line", "click"]

    # ---- 毎フレーム描き直して間隔を測る (花びらの間) ----
    class BenchFrames(renpy.Displayable):
        def __init__(self, **kw):
            super(BenchFrames, self).__init__(**kw)
            self.last = None
            self.dts = []
            self.on = False

        def render(self, width, height, st, at):
            now = time.perf_counter()
            if self.on:
                if self.last is not None:
                    self.dts.append((now - self.last) * 1000.0)
                self.last = now
                renpy.redraw(self, 0)
            return renpy.Render(1, 1)

        def start(self):
            self.dts = []
            self.last = None
            self.on = True
            renpy.redraw(self, 0)

        def stop(self):
            self.on = False
            v = sorted(self.dts)
            if v:
                pct = lambda p: v[min(len(v) - 1, int(round(p * (len(v) - 1))))]
                bench_out("BENCH rp_petals_p50 %.2f" % pct(0.5))
                bench_out("BENCH rp_petals_p95 %.2f" % pct(0.95))
                bench_out("BENCH rp_petals_frames %d" % len(v))

    bench_frames = BenchFrames()

    # ---- 自作シェーダ: 波紋 (GLSL; angle2 では ANGLE が HLSL にして D3DCompile に渡す) ----
    renpy.register_shader("bench.ripple", variables="""
        uniform float u_amount;
        uniform float u_phase;
        uniform vec2 u_model_size;
        attribute vec2 a_tex_coord;
        varying vec2 v_tex_coord;
    """, vertex_200="""
        v_tex_coord = a_tex_coord;
    """, fragment_200="""
        vec2 c = v_tex_coord - vec2(0.5);
        float d = length(c * u_model_size) / 40.0;
        vec2 uv = v_tex_coord + normalize(c + 1e-5) * sin(d - u_phase) * u_amount;
        gl_FragColor = texture2D(tex0, uv);
    """)

define e = Character(None, callback=bench_line_cb)
define typed = Character(None, callback=bench_line_cb, what_slow_cps=100,
    what_outlines=[(3, "#402030", 0, 0)], what_size=34)

transform center_ch:
    xpos 440 ypos 80 xanchor 0 yanchor 0
transform left_ch:
    xpos 40 ypos 80 xanchor 0 yanchor 0
transform right_ch:
    xpos 840 ypos 80 xanchor 0 yanchor 0

# 移動と揺れ: 横から滑らせて (三区間 x 700 ms) 画面を揺らす
transform slide_in:
    xpos -200 ypos 80 xanchor 0 yanchor 0 alpha 0.0
    easein 0.7 xpos 140 alpha 1.0
    linear 0.7 xpos 440
    linear 0.7 xpos 440

# 回想: GLSL のぼかし (多段) とセピア
transform memory:
    blur 0 matrixcolor SepiaMatrix() * SaturationMatrix(1.0)
    linear 0.6 blur 24

transform ripple:
    shader "bench.ripple" u_amount 0.02 u_phase 0.0
    linear 1.2 u_amount 0.0 u_phase 18.0

transform rotzoom:
    rotate -90 zoom 0.1 xalign 0.5 yalign 0.5 rotate_pad False
    easein 1.0 rotate 0 zoom 1.0

define wipe = ImageDissolve("rule/wipe.png", 0.6, 64)
define mosaic = Pixellate(1.0, 6)

image petals = SnowBlossom("images/petal.png", count=48, border=64, xspeed=(-80, 80), yspeed=(60, 160), start=1.0, fast=True)

# 台詞: line "本文" voice (立ち絵) (idle)
label line(t, voice, face=None, idle=False):
    if face:
        show expression face at center_ch as ch
    play sound "audio/%s.wav" % voice
    $ store._bench_kind[:] = ["line", "idle" if idle else "click"]
    e "[t]"
    return

# 場面転換: 絵を出してトランジション、終わったら合図してクリックを待つ
label scene(bg, ch=None, trans=dissolve, w="scene"):
    scene expression bg
    if ch:
        show expression ch at center_ch as ch
    with trans
    $ bench_state(w)
    $ bench_wait("click")
    $ renpy.pause()
    return

label start:
    window hide
    if bench_load:
        if renpy.can_load("1-1"):
            $ renpy.load("1-1")
        $ bench_out("BENCH-FAIL no save to load")
        $ renpy.pause(hard=True)

    scene title
    show text "{size=40}Ren'Py ベンチ　―　クリックではじめる{/size}" at truecenter
    with None
    $ bench_state("title")
    $ bench_wait("click")
    $ renpy.pause()
    hide text
    play music "audio/bgm0.wav" loop

    # ---- 一: 丘 ----
    call scene("bg1", "ch0_0")
    window show
    call line("「おはよう。今日もいい天気だね」", "voice0")
    call line("「丘の上まで行ってみない？　向こうに町が見えるはず」", "voice1", "ch0_1")
    call line("風が草をなでていく。遠くで鳥が鳴いた。", "voice2", idle=True)
    call line("「ほら、急がないと日が高くなっちゃう」", "voice3", "ch0_2")
    call line("そう言って、彼女は先に歩き出した。", "voice4")

    window hide
    show ch0_0 at slide_in as ch
    pause 2.1
    with hpunch
    $ bench_state("fx_move")
    $ bench_wait("click")
    $ renpy.pause()

    # 三人並べて一度に表情を変える
    show ch0_1 at left_ch as ch
    show ch1_1 at center_ch as ch1
    show ch2_1 at right_ch as ch2
    with Dissolve(0.4)
    $ bench_state("fx_three")
    $ bench_wait("click")
    $ renpy.pause()
    hide ch1
    hide ch2
    with Dissolve(0.3)

    # ---- 二: 夕方の道 (ルール画像で) ----
    call scene("bg2", "ch1_0", wipe)
    show petals
    show expression bench_frames as frames
    $ bench_frames.start()
    window show
    call line("「やっと追いついた。足、速すぎ」", "voice5")
    call line("「そっちが遅いだけだよ」", "voice6", "ch1_1")
    call line("いつのまにか空の色が変わり始めていた。", "voice7")
    call line("「この先で道がふたつに分かれてるんだ」", "voice8", "ch1_2")

    # 文字送り: 縁取りとルビつきの長い文を一文字ずつ (花びらは降ったまま)
    $ store._bench_kind[:] = ["fx_typed", "click"]
    typed "花びらが{rb}舞{/rb}{rt}ま{/rt}{rb}宙{/rb}{rt}ちゅう{/rt}をただよい、夕日を受けてきらきらと光っていた。ふたりはしばらく黙ったまま、その景色を眺めていた。風が吹くたびに、{rb}花弁{/rb}{rt}はなびら{/rt}はくるくると回りながら道の向こうへ流れていく。"
    $ bench_frames.stop()
    hide petals
    hide frames
    window hide

    # 回想: ぼかしてセピアに
    show bg2 at memory
    show ch1_2 at center_ch, memory as ch
    pause 0.6
    $ bench_state("fx_blur")
    $ bench_wait("click")
    $ renpy.pause()

    # 波紋 (自作シェーダ)
    scene bg2 at ripple
    show ch1_1 at center_ch as ch
    with Dissolve(0.2)
    pause 1.2
    $ bench_state("fx_ripple")
    $ bench_wait("click")
    $ renpy.pause()

    # ---- 三: 分かれ道 ----
    call scene("bg3", "ch2_0", mosaic, "fx_mosaic")
    window show
    call line("「左は森、右は川。どっちにする？」", "voice9")
    call line("「決めるのはきみだよ」", "voice10", "ch2_1")
    call line("少し考えてから、ぼくは答えた。", "voice11")
    call line("「……うん、そっちにしよう」", "voice12", "ch2_2")

    # ---- 選択肢 (ひとつめがハーネスのクリック 640,430 の下) ----
    play music "audio/bgm1.wav" loop fadeout 1.5 fadein 1.5
    window hide
    $ bench_state("fx_choice")
    $ bench_wait("choice")
    menu:
        "左の道を行く":
            scene bg4 at rotzoom
            show ch0_0 at center_ch as ch
        "右の道を行く":
            scene bg5 at rotzoom
            show ch1_0 at center_ch as ch
    pause 1.0
    $ bench_state("fx_rotatezoom")
    $ bench_wait("click")
    $ renpy.pause()

    window show
    call line("木々のあいだから、やわらかい光がこぼれていた。", "voice13")
    call line("「ここ、覚えておこう。また来たいから」", "voice14", "ch0_1")
    call line("ぼくはその景色を栞にはさむことにした。", "voice15")
    window hide

    # ---- セーブ ----
    $ renpy.save("1-1", "bench")
    $ bench_state("saved")
    $ bench_wait("click")
    $ renpy.pause()

    # ---- 動画 (4 秒の VP9 の WebM、ffmpeg で復号): クリックから終わりまで ----
    $ renpy.movie_cutscene("movie/movie.webm", stop_music=False)
    $ bench_state("fx_movie")
    $ bench_wait("click")
    $ renpy.pause()

    # ---- 四: 町 ----
    call scene("bg6", "ch2_0")
    window show
    call line("丘を下りきると、町の灯りがともり始めていた。", "voice16")
    call line("「着いたね。おなか空いた」", "voice17", "ch2_1")
    call line("ふたりで笑って、灯りのほうへ歩いていった。", "voice18")
    call line("「また明日も、どこか行こう」", "voice19", "ch2_2")

    stop music
    window hide
    scene black
    show text "おしまい" at truecenter
    with None
    $ bench_state("end")
    $ bench_out("BENCH-END")
    $ renpy.pause(hard=True)
    return

# ロードの後: セーブした場面が戻ってから restored
label after_load:
    $ renpy.pause(0.05, hard=True)
    $ bench_state("restored")
    $ bench_out("BENCH-END")
    $ renpy.pause(hard=True)
    return
