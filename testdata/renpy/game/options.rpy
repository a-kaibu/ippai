## Ren'Py ベンチ: ippai のベンチマーク (tests/bench, グループ renpy-gl / renpy-angle) が遊ぶ小さなノベル。
## 絵と音は吉里吉里ベンチ (testdata/kirikiri) のものを tests/bench/pack-renpy.mjs が images/ と audio/ に写す。

define config.name = "Ren'Py Bench"
define config.version = "1"
define config.save_directory = "renpybench"
define config.screen_width = 1280
define config.screen_height = 720
define config.window_icon = None
define config.has_autosave = False
define config.autosave_on_quit = False
define config.autosave_on_choice = False
define config.rollback_enabled = False
define config.developer = False
define config.default_fullscreen = False
define config.window_title = "Ren'Py Bench"
## (メインメニューなし: タイトルは start が出す)
define config.main_menu = []
define _game_menu_screen = None

define config.default_text_cps = 0
define config.default_afm_enable = False

style default:
    font "SourceHanSansLite.ttf"
    size 30
    color "#ffffff"
    outlines [(1, "#00000080", 0, 0)]

style ruby_style is default:
    size 14
    yoffset -26

style say_dialogue:
    ruby_style style.ruby_style
    line_leading 14
