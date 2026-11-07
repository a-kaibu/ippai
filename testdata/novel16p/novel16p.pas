{ novel16p: the second Win16 benchmark game (tests/bench, group "win16-pascal"): the same story
  as novel16 (testdata/novel16/script.txt, the same commands), but an engine of the other kind
  Windows 3.1 games were: written in Pascal (Free Pascal's i8086-win16 target, large model) and
  drawing in its own 16-bit code, not with GDI. The screen is a 640x360 256-colour picture kept in
  six bands of 60 rows (each under 64 KB: no huge pointers) that the engine composes itself:
  pictures of one shared palette (tests/bench/pack-novel16p.py), sprites with a transparent index,
  a msg_text window darkened through a colour table, transitions pixel by pixel (an ordered-dither
  dissolve, a wipe, blinds, a mosaic); every frame goes to the window as six StretchDIBits at
  twice the size. Text is TextOut over it (Shift_JIS), voices sndPlaySound.

  It says where it is on its standard output (DOS handle 1), as the other games do:
    BENCH-STATE <what> <n>, BENCH-WAIT <n> <what>, BENCH w16p_petals_frame <ms>, BENCH-END.
  "load" on the command line restores save.dat and stops there (restored).

  Build: tests/bench/build-novel16p.sh }
program novel16p;

{$mode fpc}
{$H-}

uses wintypes, winprocs;

function sndPlaySound(lpszSound: LPCSTR; fuSound: UINT): BOOL; external 'MMSYSTEM';
function timeGetTime: DWORD; external 'MMSYSTEM';

const
  SND_ASYNC = 1;
  SND_NODEFAULT = 2;
  SW = 640;           { the screen: twice as large in the window }
  SH = 360;
  BANDS = 6;
  BAND_ROWS = 60;
  MAXPICS = 32;
  CH_X: array[0..2] of SmallInt = (20, 220, 420);
  CH_Y = 40;
  TEXT_TOP = 262;     { the msg_text window's rows (in screen pixels) }
  TEXT_BOTTOM = 350;
  TRANSPARENT_INDEX = 255;

type
  PBytes = ^TBytes;
  TBytes = array[0..65000] of Byte;
  { a picture: rows top-down, in bands of BAND_ROWS rows }
  TPic = record
    name: string[15];
    w, h: SmallInt;
    band: array[0..BANDS - 1] of PBytes;
  end;
  PPic = ^TPic;
  { a screen-sized buffer: bands bottom-up, as DIBs are }
  TScreen = array[0..BANDS - 1] of PBytes;

var
  wnd: HWND;
  font: HFONT;
  bmi: record
    head: BITMAPINFOHEADER;
    colors: array[0..255] of RGBQUAD;
  end;
  dark: array[0..255] of Byte;
  box_index: Byte;
  scene, nxt, screen, src: TScreen;  { src: the mosaic's source }
  pics: array[0..MAXPICS - 1] of TPic;
  npics: SmallInt;
  bg_name: string[15];
  ch_name: array[0..2] of string[15];
  textwin: Boolean;
  msg_text: string;
  text_len: SmallInt;
  clicked, quit: Boolean;
  click_x, click_y: SmallInt;
  step: SmallInt;
  script: PBytes;
  script_size: Word;
  lines: array[0..399] of Word;
  nlines: SmallInt;

{ ---- output ---- }
procedure emit(const s: string);
var b: string;
begin
  b := s;
  _lwrite(1, @b[1], Length(b));
end;

function num(v: LongInt): string;
var s: string;
begin
  Str(v, s);
  num := s;
end;

procedure state(const what: string);
begin
  Inc(step);
  emit('BENCH-STATE ' + what + ' ' + num(step) + #10);
end;

procedure bench_wait(const what: string);
begin
  Inc(step);
  emit('BENCH-WAIT ' + num(step) + ' ' + what + #10);
end;

procedure fail(const what: string);
begin
  emit('BENCH-FAIL ' + what + #10);
  quit := True;
end;

{ ---- memory ---- }
function alloc(n: Word): PBytes;
var h: HGLOBAL;
begin
  h := GlobalAlloc(GMEM_FIXED or GMEM_ZEROINIT, n);
  if h = 0 then
  begin
    fail('out of memory');
    alloc := nil;
  end
  else
    alloc := PBytes(GlobalLock(h));
end;

procedure new_screen(var s: TScreen);
var b: SmallInt;
begin
  for b := 0 to BANDS - 1 do
    s[b] := alloc(SW * BAND_ROWS);
end;

{ row y (top-down) of a screen buffer: bands are bottom-up DIBs }
function row(var s: TScreen; y: SmallInt): PBytes;
begin
  row := PBytes(@s[y div BAND_ROWS]^[(BAND_ROWS - 1 - y mod BAND_ROWS) * SW]);
end;

function pic_row(p: PPic; y: SmallInt): PBytes;
begin
  pic_row := PBytes(@p^.band[y div BAND_ROWS]^[(y mod BAND_ROWS) * p^.w]);
end;

procedure copy_screen(var dst, src: TScreen);
var b: SmallInt;
begin
  for b := 0 to BANDS - 1 do
    Move(src[b]^, dst[b]^, SW * BAND_ROWS);
end;

{ ---- messages ---- }
procedure present; forward;
function wnd_proc(h: HWND; msg: UINT; wp: WPARAM; lp: LPARAM): LRESULT; far; export;
var ps: PAINTSTRUCT;
begin
  wnd_proc := 0;
  case msg of
    WM_LBUTTONDOWN:
      begin
        clicked := True;
        click_x := LOWORD(lp);
        click_y := HIWORD(lp);
      end;
    WM_PAINT:
      begin
        BeginPaint(h, @ps);
        EndPaint(h, @ps);
        if screen[0] <> nil then present;
      end;
    WM_DESTROY:
      begin
        quit := True;
        PostQuitMessage(0);
      end;
  else
    wnd_proc := DefWindowProc(h, msg, wp, lp);
  end;
end;

procedure pump(block: Boolean);
var m: MSG;
begin
  if block then
  begin
    if not GetMessage(@m, 0, 0, 0) then
    begin
      quit := True;
      exit;
    end;
  end
  else if not PeekMessage(@m, 0, 0, 0, PM_REMOVE) then
    exit
  else if m.message = WM_QUIT then
  begin
    quit := True;
    exit;
  end;
  TranslateMessage(@m);
  DispatchMessage(@m);
end;

procedure sleep_ms(ms: Word);
var t_end: DWORD;
    m: MSG;
begin
  t_end := timeGetTime + ms;
  if ms = 0 then ms := 1;
  SetTimer(wnd, 1, ms, nil);
  while (not quit) and (LongInt(t_end - timeGetTime) > 0) do pump(True);
  KillTimer(wnd, 1);
  while (not quit) and PeekMessage(@m, 0, 0, 0, PM_NOREMOVE) do pump(False);
end;

procedure wait_click;
begin
  clicked := False;
  while (not quit) and (not clicked) do pump(True);
end;

{ ---- the window ---- }
function is_lead(c: Char): Boolean;
begin
  is_lead := ((c >= #$81) and (c <= #$9F)) or ((c >= #$E0) and (c <= #$FC));
end;

{ the screen at (dx, dy), then the msg_text over it }
procedure present_at(dx, dy: SmallInt);
var dc: HDC;
    b, i, start, col, ln: SmallInt;
    old: HFONT;
begin
  dc := GetDC(wnd);
  for b := 0 to BANDS - 1 do
    StretchDIBits(dc, dx, dy + b * BAND_ROWS * 2, SW * 2, BAND_ROWS * 2, 0, 0, SW, BAND_ROWS,
                  screen[b], LPBITMAPINFO(@bmi), DIB_RGB_COLORS, SRCCOPY);
  if textwin and (text_len > 0) then
  begin
    old := SelectObject(dc, font);
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, RGB(255, 255, 255));
    { 40 full-width characters to a line }
    start := 1;
    col := 0;
    ln := 0;
    i := 1;
    while i <= text_len + 1 do
    begin
      if (i = text_len + 1) or (col >= 80) then
      begin
        TextOut(dc, dx + 64, dy + TEXT_TOP * 2 + 20 + ln * 38, @msg_text[start], i - start);
        Inc(ln);
        start := i;
        col := 0;
        if i = text_len + 1 then break;
      end;
      if is_lead(msg_text[i]) and (i < text_len) then
      begin
        Inc(i, 2);
        Inc(col, 2);
      end
      else
      begin
        Inc(i);
        Inc(col);
      end;
    end;
    SelectObject(dc, old);
  end;
  ReleaseDC(wnd, dc);
end;

procedure present;
begin
  present_at(0, 0);
end;

{ ---- pictures (.pic: width, height, the rows top-down, indices of the shared palette) ---- }
function load_pic(const name: string; p: PPic): Boolean;
var f: HFILE;
    path: string;
    head: array[0..1] of Word;
    b, rows: SmallInt;
begin
  load_pic := False;
  path := name + '.pic'#0;
  f := _lopen(@path[1], OF_READ);
  if f = HFILE_ERROR then
  begin
    fail(name + '.pic');
    exit;
  end;
  _lread(f, @head, 4);
  p^.name := name;
  p^.w := head[0];
  p^.h := head[1];
  for b := 0 to BANDS - 1 do
  begin
    rows := p^.h - b * BAND_ROWS;
    if rows > BAND_ROWS then rows := BAND_ROWS;
    if rows <= 0 then
      p^.band[b] := nil
    else
    begin
      p^.band[b] := alloc(Word(rows) * Word(p^.w));
      _lread(f, p^.band[b], Word(rows) * Word(p^.w));
    end;
  end;
  _lclose(f);
  load_pic := True;
end;

function pic(const name: string): PPic;
var i: SmallInt;
begin
  for i := 0 to npics - 1 do
    if pics[i].name = name then
    begin
      pic := @pics[i];
      exit;
    end;
  if npics = MAXPICS then
  begin
    fail('too many pictures');
    pic := @pics[0];
    exit;
  end;
  load_pic(name, @pics[npics]);
  pic := @pics[npics];
  Inc(npics);
end;

{ p at (x, y) on s, its transparent pixels left out, clipped to the screen }
procedure sprite(var s: TScreen; p: PPic; x, y: SmallInt);
var sy, sx, x0, x1: SmallInt;
    src, dst: PBytes;
    c: Byte;
begin
  x0 := 0;
  if x < 0 then x0 := -x;
  x1 := p^.w;
  if x + x1 > SW then x1 := SW - x;
  if x0 >= x1 then exit;
  for sy := 0 to p^.h - 1 do
  begin
    if (y + sy < 0) or (y + sy >= SH) then continue;
    src := pic_row(p, sy);
    dst := row(s, y + sy);
    for sx := x0 to x1 - 1 do
    begin
      c := src^[sx];
      if c <> TRANSPARENT_INDEX then dst^[x + sx] := c;
    end;
  end;
end;

{ a whole picture (a background) onto s }
procedure blit_bg(var s: TScreen; p: PPic);
var y: SmallInt;
begin
  for y := 0 to SH - 1 do
    Move(pic_row(p, y)^, row(s, y)^, SW);
end;

procedure compose(var s: TScreen);
var i, y: SmallInt;
begin
  if bg_name <> '' then
    blit_bg(s, pic(bg_name))
  else
    for y := 0 to SH - 1 do
      FillChar(row(s, y)^, SW, 0);
  for i := 0 to 2 do
    if ch_name[i] <> '' then sprite(s, pic(ch_name[i]), CH_X[i], CH_Y);
end;

{ the msg_text window: its rows darkened through the colour table }
procedure darken(var s: TScreen);
var y, x: SmallInt;
    r: PBytes;
begin
  if not textwin then exit;
  for y := TEXT_TOP to TEXT_BOTTOM - 1 do
  begin
    r := row(s, y);
    for x := 20 to SW - 21 do
      r^[x] := dark[r^[x]];
  end;
end;

{ ---- transitions: from screen to nxt ---- }
procedure dissolve(ms: Word);
const bayer: array[0..3, 0..3] of Byte = ((0, 8, 2, 10), (12, 4, 14, 6), (3, 11, 1, 9), (15, 7, 13, 5));
var k, y, x: SmallInt;
    src, dst: PBytes;
    t: Byte;
begin
  for k := 1 to 16 do
  begin
    if quit then exit;
    for y := 0 to SH - 1 do
    begin
      src := row(nxt, y);
      dst := row(screen, y);
      for x := 0 to SW - 1 do
      begin
        t := bayer[y and 3, x and 3];
        { (the pixels of this step's threshold) }
        if t = k - 1 then dst^[x] := src^[x];
      end;
    end;
    present;
    sleep_ms(ms div 16);
  end;
end;

procedure wipe(ms: Word);
var k, y, x0, x1: SmallInt;
begin
  x0 := 0;
  for k := 1 to 32 do
  begin
    if quit then exit;
    x1 := SW * k div 32;
    for y := 0 to SH - 1 do
      Move(row(nxt, y)^[x0], row(screen, y)^[x0], x1 - x0);
    x0 := x1;
    present;
    sleep_ms(ms div 32);
  end;
end;

procedure blinds(ms: Word);
var k, y: SmallInt;
begin
  for k := 0 to 11 do
  begin
    if quit then exit;
    y := k;
    while y < SH do
    begin
      Move(row(nxt, y)^, row(screen, y)^, SW);
      Inc(y, 12);
    end;
    present;
    sleep_ms(ms div 12);
  end;
end;

{ blocks growing over the old picture, then shrinking over the new one }
procedure mosaic(ms: Word);
const blocks: array[0..9] of SmallInt = (2, 4, 8, 16, 32, 32, 16, 8, 4, 2);
var k, b, y, x, yy: SmallInt;
    c: Byte;
    s, d: PBytes;
begin
  copy_screen(src, screen);
  for k := 0 to 9 do
  begin
    if quit then break;
    if k = 5 then copy_screen(src, nxt);
    b := blocks[k];
    y := 0;
    while y < SH do
    begin
      s := row(src, y);
      d := row(screen, y);
      x := 0;
      while x < SW do
      begin
        c := s^[x];
        if x + b <= SW then FillChar(d^[x], b, c) else FillChar(d^[x], SW - x, c);
        Inc(x, b);
      end;
      for yy := y + 1 to y + b - 1 do
        if yy < SH then Move(d^, row(screen, yy)^, SW);
      Inc(y, b);
    end;
    present;
    sleep_ms(ms div 10);
  end;
end;

procedure show(const how: string; ms: Word);
begin
  compose(scene);
  copy_screen(nxt, scene);
  darken(nxt);
  text_len := 0;
  if how = 'fade' then dissolve(ms)
  else if how = 'wipe' then wipe(ms)
  else if how = 'blinds' then blinds(ms)
  else if how = 'mosaic' then mosaic(ms);
  copy_screen(screen, nxt);
  present;
end;

{ ---- the moving parts ---- }
procedure slide(slot: SmallInt; const name: string);
var k: SmallInt;
    p: PPic;
begin
  p := pic(name);
  ch_name[slot] := '';
  compose(scene);
  for k := 0 to 30 do
  begin
    if quit then exit;
    copy_screen(screen, scene);
    sprite(screen, p, -200 + k * (CH_X[slot] + 200) div 30, CH_Y);
    present;
    sleep_ms(16);
  end;
  ch_name[slot] := name;
  compose(scene);
  copy_screen(nxt, scene);
  copy_screen(screen, nxt);
end;

procedure shake(ms: Word);
var t_end: DWORD;
    k, dx, dy: SmallInt;
begin
  t_end := timeGetTime + ms;
  k := 0;
  while (not quit) and (LongInt(t_end - timeGetTime) > 0) do
  begin
    if k and 1 <> 0 then dx := 8 else dx := -8;
    if k and 2 <> 0 then dy := 6 else dy := -6;
    present_at(dx, dy);
    Inc(k);
    sleep_ms(16);
  end;
  present;
end;

{ sine in 1/255ths, a degree a step }
function isin(deg: SmallInt): SmallInt;
const q: array[0..90] of Byte = (0, 4, 9, 13, 18, 22, 27, 31, 35, 40, 44, 49, 53, 57, 62, 66, 70, 75, 79,
  83, 87, 91, 96, 100, 104, 108, 112, 116, 120, 124, 127, 131, 135, 139, 143, 146, 150, 153, 157, 160, 164,
  167, 171, 174, 177, 180, 183, 186, 190, 192, 195, 198, 201, 204, 206, 209, 211, 214, 216, 219, 221, 223,
  225, 227, 229, 231, 233, 235, 236, 238, 240, 241, 243, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253,
  253, 254, 254, 254, 255, 255, 255, 255);
begin
  deg := deg mod 360;
  if deg < 0 then Inc(deg, 360);
  if deg <= 90 then isin := q[deg]
  else if deg <= 180 then isin := q[180 - deg]
  else if deg <= 270 then isin := -q[deg - 180]
  else isin := -q[360 - deg];
end;

procedure petals;
var p: PPic;
    f, i, x, y: SmallInt;
    t0, t: DWORD;
begin
  p := pic('petal');
  t0 := timeGetTime;
  for f := 1 to 60 do
  begin
    if quit then break;
    copy_screen(screen, nxt);
    for i := 0 to 47 do
    begin
      x := i * 13 + isin(f * 6 + i * 30) * 20 div 255;
      y := (f * 6 + i * 19) mod 400 - 32;
      sprite(screen, p, x, y);
    end;
    present;
    pump(False);
  end;
  t := timeGetTime - t0;
  emit('BENCH w16p_petals_frame ' + num(t div 60) + '.' + num((t * 10 div 60) mod 10) + #10);
  copy_screen(screen, nxt);
  present;
end;

function choice(const a, b: string): SmallInt;
const top: array[0..1] of SmallInt = (200, 260);
var i, y: SmallInt;
    dc: HDC;
    old: HFONT;
    labels: array[0..1] of string;
begin
  choice := 0;
  labels[0] := a;
  labels[1] := b;
  copy_screen(screen, scene);
  for i := 0 to 1 do
    for y := top[i] to top[i] + 29 do
      FillChar(row(screen, y)^[200], 240, box_index);
  text_len := 0;
  present;
  dc := GetDC(wnd);
  old := SelectObject(dc, font);
  SetBkMode(dc, TRANSPARENT);
  SetTextColor(dc, RGB(255, 255, 255));
  for i := 0 to 1 do
    TextOut(dc, 540, top[i] * 2 + 14, @labels[i][1], Length(labels[i]));
  SelectObject(dc, old);
  ReleaseDC(wnd, dc);
  state('fx_choice');
  bench_wait('choice');
  repeat
    wait_click;
    if quit then exit;
    for i := 0 to 1 do
      if (click_x >= 400) and (click_x < 880) and (click_y >= top[i] * 2) and (click_y < top[i] * 2 + 60) then
      begin
        choice := i;
        exit;
      end;
  until False;
end;

{ ---- saves ---- }
function dash(const s: string): string;
begin
  if s = '' then dash := '-' else dash := s;
end;

procedure save;
var f: HFILE;
    s: string;
begin
  f := _lcreat('save.dat', 0);
  if f = HFILE_ERROR then
  begin
    fail('save.dat');
    exit;
  end;
  s := dash(bg_name) + ' ' + dash(ch_name[0]) + ' ' + dash(ch_name[1]) + ' ' + dash(ch_name[2]) + #10;
  _lwrite(f, @s[1], Length(s));
  _lclose(f);
end;

{ the nxt space-separated word of s from i }
function next_word(const s: string; var i: SmallInt): string;
var w: string;
begin
  w := '';
  while (i <= Length(s)) and (s[i] = ' ') do Inc(i);
  while (i <= Length(s)) and (s[i] > ' ') do
  begin
    w := w + s[i];
    Inc(i);
  end;
  next_word := w;
end;

function undash(const s: string): string;
begin
  if s = '-' then undash := '' else undash := s;
end;

function restore: Boolean;
var f: HFILE;
    s: string;
    n, i, k: SmallInt;
begin
  restore := False;
  f := _lopen('save.dat', OF_READ);
  if f = HFILE_ERROR then exit;
  n := _lread(f, @s[1], 250);
  _lclose(f);
  if n < 0 then n := 0;
  SetLength(s, n);
  i := 1;
  bg_name := undash(next_word(s, i));
  for k := 0 to 2 do
    ch_name[k] := undash(next_word(s, i));
  restore := True;
end;

{ ---- the script ---- }
procedure load_script;
var f: HFILE;
    size: LongInt;
    i: Word;
begin
  f := _lopen('script.txt', OF_READ);
  if f = HFILE_ERROR then
  begin
    fail('no script.txt');
    exit;
  end;
  size := _llseek(f, 0, 2);
  _llseek(f, 0, 0);
  script := alloc(size + 1);
  script_size := _lread(f, script, size);
  _lclose(f);
  i := 0;
  while (i < script_size) and (nlines < 400) do
  begin
    lines[nlines] := i;
    Inc(nlines);
    while (i < script_size) and (script^[i] <> 13) and (script^[i] <> 10) do Inc(i);
    while (i < script_size) and ((script^[i] = 13) or (script^[i] = 10)) do
    begin
      script^[i] := 0;
      Inc(i);
    end;
  end;
end;

function line(n: SmallInt): string;
var s: string;
    i: Word;
begin
  s := '';
  i := lines[n];
  while (i < script_size) and (script^[i] <> 0) and (Length(s) < 255) do
  begin
    s := s + Chr(script^[i]);
    Inc(i);
  end;
  line := s;
end;

function find_label(const name: string): SmallInt;
var i: SmallInt;
begin
  for i := 0 to nlines - 1 do
    if line(i) = '*' + name then
    begin
      find_label := i;
      exit;
    end;
  fail('no such label');
  find_label := nlines;
end;

procedure say(const t: string; ms: Word);
begin
  msg_text := t;
  text_len := 0;
  while (text_len < Length(msg_text)) and not quit do
  begin
    if is_lead(msg_text[text_len + 1]) then Inc(text_len, 2) else Inc(text_len);
    present;
    sleep_ms(ms);
  end;
end;

function to_int(const s: string): SmallInt;
var v, code: SmallInt;
begin
  Val(s, v, code);
  if code <> 0 then v := 0;
  to_int := v;
end;

procedure run(load_mode: Boolean);
var pc, i: SmallInt;
    ms: Word;
    s, cmd, a, b, rest: string;
    path: string;
begin
  ms := 30;
  if load_mode then
  begin
    if not restore then
    begin
      fail('no save to load');
      exit;
    end;
    show('cut', 0);
    { （ロードしました） }
    say(#$81#$69#$83#$8D#$81#$5B#$83#$68#$82#$B5#$82#$DC#$82#$B5#$82#$BD#$81#$6A, 0);
    state('restored');
    emit('BENCH-END'#10);
    exit;
  end;
  pc := 0;
  while (pc < nlines) and not quit do
  begin
    s := line(pc);
    Inc(pc);
    if (s = '') or (s[1] = ';') or (s[1] = '*') then continue;
    i := 1;
    cmd := next_word(s, i);
    while (i <= Length(s)) and (s[i] = ' ') do Inc(i);
    rest := Copy(s, i, 255);
    if cmd = 'bg' then bg_name := undash(rest)
    else if cmd = 'show' then
    begin
      a := next_word(s, i);
      show(a, to_int(next_word(s, i)));
    end
    else if cmd = 'ch' then
    begin
      a := next_word(s, i);
      b := next_word(s, i);
      ch_name[Ord(a[1]) - Ord('0')] := undash(b);
    end
    else if cmd = 'voice' then
    begin
      path := 'sound\' + rest + '.wav'#0;
      sndPlaySound(@path[1], SND_ASYNC or SND_NODEFAULT);
    end
    else if cmd = 'speed' then ms := to_int(rest)
    else if cmd = 'say' then say(rest, ms)
    else if cmd = 'textwin' then
    begin
      textwin := rest = 'on';
      copy_screen(nxt, scene);
      darken(nxt);
      copy_screen(screen, nxt);
      present;
    end
    else if cmd = 'st' then state(rest)
    else if cmd = 'bw' then
    begin
      bench_wait(rest);
      wait_click;
    end
    else if cmd = 'slide' then
    begin
      a := next_word(s, i);
      slide(Ord(a[1]) - Ord('0'), next_word(s, i));
    end
    else if cmd = 'shake' then shake(to_int(rest))
    else if cmd = 'petals' then petals
    else if cmd = 'choice' then
    begin
      a := line(pc);
      b := line(pc + 1);
      Inc(pc, 2);
      if choice(a, b) = 1 then pc := find_label(rest);
    end
    else if cmd = 'goto' then pc := find_label(rest)
    else if cmd = 'save' then save
    else if cmd = 'end' then
    begin
      emit('BENCH-END'#10);
      exit;
    end
    else fail('unknown command ' + cmd);
  end;
end;

{ ---- start ---- }
procedure load_palette;
var f: HFILE;
    best, d, bd, i: LongInt;
begin
  f := _lopen('pal.bin', OF_READ);
  if f = HFILE_ERROR then
  begin
    fail('no pal.bin');
    exit;
  end;
  _lread(f, @bmi.colors, 1024);
  _lread(f, @dark, 256);
  _lclose(f);
  FillChar(bmi.head, SizeOf(bmi.head), 0);
  bmi.head.biSize := SizeOf(BITMAPINFOHEADER);
  bmi.head.biWidth := SW;
  bmi.head.biHeight := BAND_ROWS;
  bmi.head.biPlanes := 1;
  bmi.head.biBitCount := 8;
  bmi.head.biCompression := BI_RGB;
  bmi.head.biClrUsed := 256;
  { the choice buttons' colour: the palette's nearest to a dark blue }
  bd := $7FFFFFFF;
  best := 0;
  for i := 0 to 254 do
  begin
    d := Sqr(LongInt(bmi.colors[i].rgbRed) - 40) + Sqr(LongInt(bmi.colors[i].rgbGreen) - 40) + Sqr(LongInt(bmi.colors[i].rgbBlue) - 90);
    if d < bd then
    begin
      bd := d;
      best := i;
    end;
  end;
  box_index := best;
end;

var
  wc: WNDCLASS;
  cmd: string;
  p: PChar;
begin
  textwin := True;
  if HPrevInst = 0 then
  begin
    FillChar(wc, SizeOf(wc), 0);
    wc.lpfnWndProc := @wnd_proc;
    wc.hInstance := HInstance;
    wc.hCursor := LoadCursor(0, IDC_ARROW);
    wc.hbrBackground := GetStockObject(BLACK_BRUSH);
    wc.lpszClassName := 'novel16p';
    RegisterClass(@wc);
  end;
  wnd := CreateWindow('novel16p', 'novel16p', WS_POPUP or WS_VISIBLE, 0, 0, SW * 2, SH * 2, 0, 0, HInstance, nil);
  { ＭＳ ゴシック, 28 px }
  font := CreateFont(-28, 0, 0, 0, FW_NORMAL, 0, 0, 0, SHIFTJIS_CHARSET, 0, 0, 0, FIXED_PITCH,
                     #$82#$6C#$82#$72' '#$83#$53#$83#$56#$83#$62#$83#$4E);
  ShowWindow(wnd, SW_SHOW);
  UpdateWindow(wnd);
  load_palette;
  new_screen(scene);
  new_screen(nxt);
  new_screen(screen);
  new_screen(src);
  load_script;
  cmd := '';
  p := CmdLine;
  while (p <> nil) and (p^ <> #0) do
  begin
    if p^ > ' ' then cmd := cmd + p^;
    Inc(p);
  end;
  if not quit then run(cmd = 'load');
  while not quit do pump(True);
end.
