// logs.html: the runs' logs (web/src/crashlog.ts) on the server (ippai serve GET /log-list,
// files under /logs/) and in this browser's OPFS, to read, download or send.

import { useEffect, useState, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { toast } from "sonner";
import { ArrowLeft, Download, ExternalLink, Eye, FileWarning, Send } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { Toaster } from "@/components/ui/sonner";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { listLocal, readLocal, sendLeftovers, upload } from "./crashlog";

interface LogFile {
  name: string;
  size: number;
  time: number;
}

function kind(name: string): "crash" | "unclean" | null {
  return /-crash\d*(\.txt)?$/.test(name) ? "crash" : /-unclean(\.txt)?$/.test(name) ? "unclean" : null;
}

function download(name: string, text: string): void {
  const a = document.createElement("a");
  a.href = URL.createObjectURL(new Blob([text], { type: "text/plain;charset=utf-8" }));
  a.download = name.endsWith(".txt") ? name : `${name}.txt`;
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 10_000);
}

function Row({ file, children }: { file: LogFile; children: ReactNode }) {
  const k = kind(file.name);
  return (
    <li className="flex flex-wrap items-center gap-x-3 gap-y-1.5 border-b py-2.5 last:border-0">
      <div className="min-w-0 flex-[1_1_18rem]">
        <div className="flex items-center gap-2">
          {k && <FileWarning className="size-4 shrink-0 text-destructive" />}
          <span className={k ? "break-all text-destructive" : "break-all"}>{file.name}</span>
          {k && (
            <Badge variant="destructive" className="shrink-0">
              {k === "crash" ? "クラッシュ" : "異常終了"}
            </Badge>
          )}
        </div>
        <div className="mt-0.5 text-xs text-muted-foreground">
          {new Date(file.time).toLocaleString("ja-JP")} · {(file.size / 1024).toFixed(0)} KB
        </div>
      </div>
      <div className="flex flex-wrap gap-1.5">{children}</div>
    </li>
  );
}

function List({ files, children }: { files: LogFile[] | null | Error; children: (f: LogFile) => ReactNode }) {
  if (files === null)
    return (
      <div className="grid gap-2">
        {[0, 1, 2].map((i) => (
          <Skeleton key={i} className="h-12" />
        ))}
      </div>
    );
  if (files instanceof Error)
    return <p className="text-sm text-destructive">一覧を取れませんでした: {files.message}</p>;
  if (!files.length) return <p className="text-sm text-muted-foreground">まだありません</p>;
  return (
    <ul className="rounded-xl border bg-card px-4">
      {files.map((f) => (
        <Row key={f.name} file={f}>
          {children(f)}
        </Row>
      ))}
    </ul>
  );
}

async function serverList(): Promise<LogFile[]> {
  const r = await fetch("log-list");
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return (await r.json()) as LogFile[];
}

function Logs() {
  const [server, setServer] = useState<LogFile[] | null | Error>(null);
  const [local, setLocal] = useState<LogFile[] | null | Error>(null);
  const [view, setView] = useState<{ name: string; text: string } | null>(null);
  const [sent, setSent] = useState<Record<string, boolean>>({});
  const reloadServer = () => serverList().then(setServer, (e: Error) => setServer(e));
  useEffect(() => {
    void sendLeftovers()
      .catch(() => {})
      .finally(() => {
        void reloadServer();
        listLocal().then(setLocal, (e: Error) => setLocal(e));
      });
  }, []);
  const send = async (f: LogFile) => {
    const ok = await upload(f.name.replace(/\.txt$/, ""), await readLocal(f.name));
    setSent((s) => ({ ...s, [f.name]: ok }));
    if (ok) {
      toast.success(`${f.name} をサーバーへ送りました`);
      void reloadServer();
    } else toast.error(`${f.name} を送れませんでした`);
  };
  return (
    <div className="min-h-dvh bg-background">
      <header className="sticky top-0 z-10 border-b bg-background/80 backdrop-blur">
        <div className="mx-auto flex max-w-4xl items-center gap-2 px-4 py-3">
          <Button asChild variant="ghost" size="icon-sm" aria-label="ゲーム一覧へ">
            <a href="./">
              <ArrowLeft />
            </a>
          </Button>
          <h1 className="text-lg font-bold tracking-tight">ログ</h1>
        </div>
      </header>
      <main className="mx-auto grid max-w-4xl gap-4 px-4 pt-4 pb-10">
        <p className="text-sm leading-relaxed text-muted-foreground">
          ゲームの実行ごとの詳細ログです。クラッシュ時・バックグラウンド移行時・終了時に保存され、サーバーにも送られます。プレイ中は{" "}
          <kbd className="rounded border bg-muted px-1 text-xs">Ctrl+Shift+L</kbd> か 3
          本指タップで手動保存できます。
        </p>
        <Tabs defaultValue="server">
          <TabsList>
            <TabsTrigger value="server">サーバー</TabsTrigger>
            <TabsTrigger value="local">このブラウザ (OPFS)</TabsTrigger>
          </TabsList>
          <TabsContent value="server">
            <List files={server}>
              {(f) => (
                <Button asChild size="sm" variant="outline">
                  <a href={`logs/${encodeURIComponent(f.name)}`} target="_blank" rel="noopener">
                    開く <ExternalLink className="size-3" />
                  </a>
                </Button>
              )}
            </List>
          </TabsContent>
          <TabsContent value="local">
            <List files={local}>
              {(f) => (
                <>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={async () => setView({ name: f.name, text: await readLocal(f.name) })}
                  >
                    <Eye /> 表示
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={async () => download(f.name, await readLocal(f.name))}
                  >
                    <Download /> ダウンロード
                  </Button>
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={f.name in sent}
                    onClick={() => void send(f)}
                  >
                    <Send />{" "}
                    {f.name in sent ? (sent[f.name] ? "送信済み" : "送れませんでした") : "サーバーへ送る"}
                  </Button>
                </>
              )}
            </List>
          </TabsContent>
        </Tabs>
      </main>
      {view && (
        <Dialog open onOpenChange={(o) => !o && setView(null)}>
          <DialogContent className="max-w-4xl p-5">
            <DialogHeader>
              <DialogTitle className="pr-8 break-all">{view.name}</DialogTitle>
              <DialogDescription>このブラウザに保存されたログ</DialogDescription>
            </DialogHeader>
            <pre className="mt-3 max-h-[70dvh] overflow-auto rounded-lg bg-muted p-3 font-mono text-xs leading-snug break-all whitespace-pre-wrap">
              {view.text}
            </pre>
          </DialogContent>
        </Dialog>
      )}
      <Toaster position="bottom-center" />
    </div>
  );
}

createRoot(document.querySelector("#ui")!).render(<Logs />);
