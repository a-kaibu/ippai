------------------------------- MODULE ComRefs -------------------------------
(* What Direct3D 9 and 11 say of their objects' lifetimes and reference counts, as kura's object
   tables (d3d9/mod.rs, d3d11/mod.rs) must follow them; ComRefsTrace checks a trace of the real
   tables against it, step by step.

   An object lives while something reaches it: the application's references, an object that holds
   one (a view its resource, a device its IDirect3D9, a swap chain its buffer), or a binding of a
   live object (what a device's state, a state block or a swap chain binds). A texture's levels
   live as long as their texture, and their references and bindings count on it. AddRef and
   Release return the public count: the application's references to the group plus those live objects hold.

   As Windows reports them (Wine's d3d11 tests, tests/winref d3d11api): a D3D11 view holds its
   resource privately (the resource's count does not change), and every D3D11 device child (the
   immediate context included) holds a public reference on the device while it has public
   references itself. The immediate context starts with none.

   Left out on purpose, as kura does them: D3D9 resources do not hold a reference on their device
   (a game that releases its device first destroys it at once), and Reset and ResizeBuffers do not
   fail while the application still holds the old buffers (those buffers live on, unbound). *)
EXTENDS Naturals, Sequences, FiniteSets

\* (everything a device, a context, a state block or a swap chain binds)
Slots == {"tex0", "tex1", "vb0", "rt0", "rt1", "ds", "bb", "sc", "ads",
          "srv0", "srv1", "rtv0", "dsv", "vb", "ib", "cb0", "buf"}
\* (what a D3D9 state block captures)
Captured == {"tex0", "tex1", "vb0"}
\* (D3D11: what ClearState unbinds)
Ctx == {"srv0", "srv1", "rtv0", "dsv", "vb", "ib", "cb0"}
\* (live as long as the process: the D3D11 device, context and DXGI objects)
Permanent == {"device11", "context", "factory"}

NoSlots == [s \in Slots |-> 0]

\* State: known ids and, for each, kind, container (0: none), levels, the application's
\* references, what it holds a reference to, what it binds; the live ones; the last call's result
\* and the object it handed back.
Empty == [known |-> {}, kind |-> <<>>, parent |-> <<>>, lv |-> <<>>, app |-> <<>>,
          held |-> <<>>, slots |-> <<>>, alive |-> {}, ret |-> "ok", got |-> 0]

Group(st, o) == IF st.parent[o] = 0 THEN o ELSE st.parent[o]

RECURSIVE SumApp(_, _)
SumApp(st, S) == IF S = {} THEN 0 ELSE LET o == CHOOSE x \in S : TRUE IN st.app[o] + SumApp(st, S \ {o})

AppCount(st, g) == SumApp(st, {o \in st.known : Group(st, o) = g})

\* (D3D11 objects whose references are private: views on their resources)
Views == {"view"}
\* (D3D11 device children: they hold the device while they have public references)
Children11 == {"texture", "buffer", "view", "context"}

\* (the public references on g: the application's and those live objects hold, but views')
Pub(st, g) == AppCount(st, g) + Cardinality({h \in st.alive : g \in st.held[h] /\ st.kind[h] \notin Views})

Refs(st, g) ==
    Pub(st, g) + IF st.kind[g] = "device11"
                 THEN Cardinality({h \in st.alive : st.kind[h] \in Children11 /\ st.parent[h] = 0 /\ Pub(st, h) > 0})
                 ELSE 0

Reaches(st, S, o) ==
    IF st.parent[o] # 0 THEN st.parent[o] \in S
    ELSE \/ st.kind[o] \in Permanent
         \/ AppCount(st, o) > 0
         \/ \E h \in S : \E x \in st.held[h] : x \in st.known /\ Group(st, x) = o
         \/ \E h \in S : \E s \in Slots : st.slots[h][s] \in st.known /\ Group(st, st.slots[h][s]) = o

RECURSIVE Fix(_, _, _)
Fix(st, cand, S) ==
    LET T == {o \in cand : Reaches(st, S, o)} IN IF T = S THEN S ELSE Fix(st, cand, T)

\* The objects left alive (none comes back: candidates are the live ones and the new ones).
Collect(st, new) == [st EXCEPT !.alive = Fix(st, st.alive \cup new, {})]

Ext(f, ids, v) == [x \in DOMAIN f \cup ids |-> IF x \in ids THEN v ELSE f[x]]

\* New objects `ids` of kind k with container p (0: none), holding refs `h`, binding `sl`;
\* the application gets a reference to the first one.
Make(st, ids, k, p, h, sl) ==
    LET s == {ids[i] : i \in DOMAIN ids} \ {0}
    IN [st EXCEPT !.known = @ \cup s,
                  !.kind = Ext(@, s, k),
                  !.parent = Ext(@, s, p),
                  !.lv = Ext(@, s, <<>>),
                  !.app = Ext(@, s, 0),
                  !.held = Ext(@, s, h),
                  !.slots = Ext(@, s, sl),
                  !.alive = @ \cup s]

GiveApp(st, o) == IF o = 0 THEN st ELSE [st EXCEPT !.app[o] = @ + 1]

\* A device's buffers: back buffer, swap chain, automatic depth (0: none).
Buffers(st, t) ==
    LET a == Make(Make(Make(st, <<t.bb>>, "surface", 0, {}, NoSlots), <<t.sc>>, "swapchain", 0, {}, NoSlots),
                  <<t.ads>>, "surface", 0, {}, NoSlots)
    IN a

Done(st, ret, got) == [Collect(st, {}) EXCEPT !.ret = ret, !.got = got]

Do(st, t) ==
    CASE t.op = "CreateD3D" ->
            Done(GiveApp(Make(st, <<t.o>>, "d3d", 0, {}, NoSlots), t.o), "ok", t.o)
      [] t.op = "CreateDevice" ->
            LET a == Buffers(GiveApp(Make(st, <<t.o>>, "device", 0, {t.d3d}, NoSlots), t.o), t)
                sl == [NoSlots EXCEPT !["bb"] = t.bb, !["sc"] = t.sc, !["ads"] = t.ads, !["rt0"] = t.bb, !["ds"] = t.ads]
            IN Done([a EXCEPT !.slots[t.o] = sl], "ok", t.o)
      [] t.op = "Reset" ->
            LET a == Buffers(st, t)
                sl == [NoSlots EXCEPT !["bb"] = t.bb, !["sc"] = t.sc, !["ads"] = t.ads, !["rt0"] = t.bb, !["ds"] = t.ads]
            IN Done([a EXCEPT !.slots[t.dev] = sl], "ok", 0)
      [] t.op = "CreateDevice11" ->
            \* (the device t.dev and its context t.sb; the swap chain t.o holds its buffer t.v and
            \* the device)
            LET a == Make(Make(st, <<t.sb>>, "context", 0, {}, NoSlots), <<t.dev>>, "device11", 0, {}, NoSlots)
                b == Make(Make(a, <<t.v>>, "texture", 0, {}, NoSlots), <<t.o>>, "swapchain", 0, {t.v, t.dev}, NoSlots)
            IN Done(GiveApp(GiveApp(GiveApp(b, t.dev), t.sb), t.o), "ok", t.o)
      [] t.op = "CreateTexture" ->
            LET a == Make(GiveApp(Make(st, <<t.o>>, "texture", 0, {}, NoSlots), t.o), t.lv, "surface", t.o, {}, NoSlots)
            IN Done([a EXCEPT !.lv[t.o] = t.lv], "ok", t.o)
      [] t.op = "Create" ->
            \* (a surface, a buffer: kind t.slot; a view of t.v, which it holds)
            Done(GiveApp(Make(st, <<t.o>>, t.slot, 0, IF t.v = 0 THEN {} ELSE {t.v}, NoSlots), t.o), "ok", t.o)
      [] t.op = "CreateStateBlock" ->
            LET sl == [s \in Slots |-> IF s \in Captured THEN st.slots[t.dev][s] ELSE 0]
            IN Done(GiveApp(Make(st, <<t.o>>, "stateblock", 0, {}, sl), t.o), "ok", t.o)
      [] t.op = "Capture" ->
            Done([st EXCEPT !.slots[t.sb] = [s \in Slots |-> IF s \in Captured THEN st.slots[t.dev][s] ELSE 0]], "ok", 0)
      [] t.op = "Apply" ->
            Done([st EXCEPT !.slots[t.dev] = [s \in Slots |-> IF s \in Captured THEN st.slots[t.sb][s] ELSE @[s]]], "ok", 0)
      [] t.op = "Set" ->
            IF t.slot = "rt0" /\ t.v = 0 THEN Done(st, "invalid", 0)
            ELSE Done([st EXCEPT !.slots[t.dev][t.slot] = t.v], "ok", 0)
      [] t.op = "Get" ->
            LET o == st.slots[t.dev][t.slot]
            IN IF o = 0 /\ t.slot \in {"rt0", "rt1", "dsv"} THEN Done(st, "notfound", 0)
               ELSE Done(GiveApp(st, o), "ok", o)
      [] t.op = "GetLevel" ->
            IF t.l < Len(st.lv[t.o]) THEN LET o == st.lv[t.o][t.l + 1] IN Done(GiveApp(st, o), "ok", o)
            ELSE Done(st, "invalid", 0)
      [] t.op = "GetHeld" ->
            \* (a swap chain's buffer, a view's resource)
            LET o == CHOOSE x \in st.held[t.o] : st.kind[x] # "device11" IN Done(GiveApp(st, o), "ok", o)
      [] t.op = "ResizeBuffers" ->
            \* (the new buffer: the swap chain's only reference to it)
            Done([Make(st, <<t.v>>, "texture", 0, {}, NoSlots) EXCEPT
                    !.held[t.o] = {t.v} \cup {x \in @ : st.kind[x] = "device11"}], "ok", 0)
      [] t.op = "ClearState" ->
            Done([st EXCEPT !.slots[t.dev] = [s \in Slots |-> IF s \in Ctx THEN 0 ELSE @[s]]], "ok", 0)
      [] t.op = "QueryInterface" ->
            Done(GiveApp(st, t.o), "ok", t.o)
      [] t.op = "AddRef" ->
            LET a == GiveApp(st, t.o) IN Done(a, Refs(a, Group(a, t.o)), 0)
      [] t.op = "Release" ->
            LET a == Collect([st EXCEPT !.app[t.o] = @ - 1], {})
            IN Done(a, Refs(a, Group(a, t.o)), 0)
=============================================================================
