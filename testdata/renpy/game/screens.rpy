## 台詞と選択肢だけの最小の画面 (メッセージ枠 40,520 1200x180、選択肢のひとつめはハーネスのクリック 640,430 の下)

screen say(who, what):
    window:
        id "window"
        xpos 40 ypos 520 xsize 1200 ysize 180
        background Solid("#000000a0")
        padding (24, 20)
        text what id "what" style "say_dialogue"

screen choice(items):
    add Solid("#00000060")
    vbox:
        xpos 400 ypos 400 spacing 40
        for i in items:
            textbutton i.caption action i.action:
                xsize 480 ysize 80
                background Solid("#30305080")
                hover_background Solid("#5050a0a0")
                text_size 44

## (ゲームメニュー・確認・通知は出さない)
screen confirm(message, yes_action, no_action):
    timer 0.01 action yes_action

screen notify(message):
    pass
