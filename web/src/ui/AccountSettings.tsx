import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { clearAccountSettings, loadAccountSettings, saveAccountSettings } from "../account-settings";

/** 設定's DMM アカウント section (ui/Settings.tsx). */
export function AccountSettings() {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [remember, setRemember] = useState(false);
  const [show, setShow] = useState(false);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  const [status, setStatus] = useState("");
  useEffect(() => {
    let active = true;
    void loadAccountSettings()
      .then((saved) => {
        if (!active) return;
        setEmail(saved.email);
        setPassword(saved.password);
        setRemember(saved.rememberPassword);
      })
      .catch(() => {
        if (active) setError("設定を読み込めませんでした。ブラウザのストレージ設定を確認してください。");
      })
      .finally(() => {
        if (active) setBusy(false);
      });
    return () => {
      active = false;
    };
  }, []);
  async function update(remove: boolean) {
    setBusy(true);
    setError("");
    setStatus("");
    try {
      if (remove) {
        await clearAccountSettings();
        setEmail("");
        setPassword("");
        setRemember(false);
        setShow(false);
      } else {
        await saveAccountSettings({ email, password, rememberPassword: remember });
      }
      setStatus(
        remove
          ? "保存したアカウント情報を削除しました。"
          : "設定を保存しました。対応ゲームの起動時に、公式の認証画面へ入力して送信します。",
      );
    } catch {
      setError("設定を保存できませんでした。HTTPS 接続とブラウザのストレージ設定を確認してください。");
    } finally {
      setBusy(false);
    }
  }
  return (
    <form
      className="grid gap-4"
      aria-busy={busy}
      onSubmit={(event) => {
        event.preventDefault();
        void update(false);
      }}
    >
      <p className="text-sm text-muted-foreground">
        ソフト電池の正規認証に使うアカウントです。対応ゲームの起動時に、公式の認証画面へ入力して送信します。
      </p>
      <fieldset disabled={busy} className="grid min-w-0 gap-4">
        <div className="grid gap-2">
          <label htmlFor="dmm-email" className="text-sm font-medium">
            メールアドレス
          </label>
          <Input
            id="dmm-email"
            name="username"
            type="email"
            autoComplete="username"
            autoCapitalize="none"
            spellCheck={false}
            value={email}
            onChange={(e) => setEmail(e.target.value)}
          />
        </div>
        <div className="grid gap-2">
          <label htmlFor="dmm-password" className="text-sm font-medium">
            パスワード
          </label>
          <Input
            id="dmm-password"
            name="password"
            type={show ? "text" : "password"}
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            aria-describedby="dmm-storage"
          />
          <label className="flex min-h-10 items-center gap-2 text-sm">
            <input type="checkbox" checked={show} onChange={(e) => setShow(e.target.checked)} />
            パスワードを表示
          </label>
        </div>
        <label className="flex min-h-10 items-center gap-2 text-sm">
          <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} />
          このブラウザにパスワードを保存
        </label>
        <p id="dmm-storage" className="text-sm text-muted-foreground">
          メールアドレスはこのブラウザに保存します。パスワードはページを閉じるまで使えます。保存を選ぶと暗号化して保持します。別端末には同期しません。
        </p>
      </fieldset>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <p role="status" className="text-sm text-muted-foreground">
        {busy ? "処理中…" : status}
      </p>
      <div className="flex flex-wrap justify-end gap-2">
        <Button type="button" variant="outline" disabled={busy} onClick={() => void update(true)}>
          保存情報を削除
        </Button>
        <Button type="submit" disabled={busy}>
          保存
        </Button>
      </div>
    </form>
  );
}
