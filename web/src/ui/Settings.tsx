// 設定: a sidebar on the right of the game list (not modal: the list stays in use beside it on a
// wide screen), with the server (its version and an update to the newest release), the app
// (installing the page as an app), the play statistics (with a reset) and the DMM account.

import { useEffect, useState, type ReactNode } from "react";
import { BarChart3, CircleCheck, Loader2, MonitorDown, RefreshCw, Server, UserRound } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet";
import type { GameEntry } from "../launcher";
import { useApp } from "../pwa";
import { checkUpdate, installUpdate, waitForVersion, type UpdateInfo } from "../server-update";
import { AccountSettings } from "./AccountSettings";
import { Statistics } from "./Personal";

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

function Part({ title, icon, children }: { title: string; icon: ReactNode; children: ReactNode }) {
  return (
    <section aria-label={title} className="grid gap-3 border-b px-5 py-5 last:border-b-0">
      <h3 className="flex items-center gap-2 text-sm font-semibold [&_svg]:size-4 [&_svg]:text-muted-foreground">
        {icon}
        {title}
      </h3>
      {children}
    </section>
  );
}

type Step =
  | { s: "checking" }
  | { s: "ready"; info: UpdateInfo }
  | { s: "installing" }
  | { s: "restarting"; version: string }
  | { s: "failed"; error: string; info?: UpdateInfo };

/** The running server's version, and 更新 when a newer release is out. */
function ServerUpdate({ build }: { build: string }) {
  const [step, setStep] = useState<Step>({ s: "checking" });
  const check = () => {
    setStep({ s: "checking" });
    checkUpdate().then(
      (info) => setStep({ s: "ready", info }),
      (e: unknown) => setStep({ s: "failed", error: message(e) }),
    );
  };
  useEffect(check, []);
  const update = async (info: UpdateInfo) => {
    setStep({ s: "installing" });
    try {
      const version = await installUpdate();
      setStep({ s: "restarting", version });
      if (!(await waitForVersion(version)))
        return setStep({
          s: "failed",
          error: "サーバーが再起動から戻りません。しばらくして再読み込みしてください",
          info,
        });
      // (the page's service worker takes the new build's files on the reload)
      await navigator.serviceWorker
        ?.getRegistration()
        .then((r) => r?.update())
        .catch(() => undefined);
      location.reload();
    } catch (e) {
      setStep({ s: "failed", error: message(e), info });
    }
  };
  const info = step.s === "ready" ? step.info : step.s === "failed" ? step.info : undefined;
  return (
    <div className="grid gap-2 text-sm" data-testid="server-update">
      <p>
        動作中のサーバー: <span className="font-medium tabular-nums">{build || info?.current || "不明"}</span>
      </p>
      {step.s === "checking" && (
        <p role="status" className="flex items-center gap-2 text-muted-foreground">
          <Loader2 className="size-4 animate-spin" /> 最新版を確認しています…
        </p>
      )}
      {info && !info.newer && !info.error && info.latest && (
        <p className="flex items-center gap-2 text-muted-foreground">
          <CircleCheck className="size-4 text-emerald-600" /> 最新版です（v{info.latest.replace(/^v/, "")}）
        </p>
      )}
      {info?.error && <p className="text-muted-foreground">最新版を確認できませんでした: {info.error}</p>}
      {info?.newer && (
        <>
          <p>
            新しいバージョン{" "}
            <span className="font-medium tabular-nums">v{info.latest.replace(/^v/, "")}</span> があります。
          </p>
          {info.supported ? (
            <Button className="justify-self-start" onClick={() => void update(info)}>
              <RefreshCw /> v{info.latest.replace(/^v/, "")} に更新
            </Button>
          ) : (
            <p className="text-muted-foreground">このサーバーでは自動更新できません（Linux のみ）。</p>
          )}
        </>
      )}
      {step.s === "installing" && (
        <p role="status" className="flex items-center gap-2">
          <Loader2 className="size-4 animate-spin" /> ダウンロードして入れ替えています…
        </p>
      )}
      {step.s === "restarting" && (
        <p role="status" className="flex items-center gap-2">
          <Loader2 className="size-4 animate-spin" /> v{step.version.replace(/^v/, "")} で再起動しています…
        </p>
      )}
      {step.s === "failed" && (
        <p role="alert" className="text-destructive">
          {step.error}
        </p>
      )}
      {(step.s === "ready" || step.s === "failed") && (
        <Button size="sm" variant="ghost" className="justify-self-start" onClick={check}>
          <RefreshCw /> もう一度確認
        </Button>
      )}
    </div>
  );
}

export function Settings({
  games,
  build,
  onClose,
  returnFocus,
}: {
  games: GameEntry[];
  build: string;
  onClose: () => void;
  returnFocus: () => void;
}) {
  const app = useApp();
  return (
    <Sheet
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <SheetContent
        onInteractOutside={(e) => e.preventDefault()}
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          returnFocus();
        }}
      >
        <div className="border-b px-5 py-4 pr-12">
          <SheetTitle>設定</SheetTitle>
          <SheetDescription>サーバー・アプリ・プレイ統計・アカウント</SheetDescription>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
          <Part title="サーバー" icon={<Server />}>
            <ServerUpdate build={build} />
          </Part>
          <Part title="アプリ" icon={<MonitorDown />}>
            {app.install ? (
              <>
                <p className="text-sm text-muted-foreground">
                  ホーム画面やデスクトップに ippai をアプリとして追加します。
                </p>
                <Button variant="outline" className="justify-self-start" onClick={app.install}>
                  <MonitorDown /> アプリにする
                </Button>
              </>
            ) : (
              <p className="text-sm text-muted-foreground">
                アプリとして追加済みか、このブラウザでは追加できません（ブラウザのメニューから追加できる場合があります）。
              </p>
            )}
          </Part>
          <Part title="プレイ統計" icon={<BarChart3 />}>
            <Statistics games={games} />
          </Part>
          <Part title="DMM アカウント" icon={<UserRound />}>
            <AccountSettings />
          </Part>
        </div>
      </SheetContent>
    </Sheet>
  );
}
