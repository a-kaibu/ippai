import { expect, test } from "vite-plus/test";
import { preloadFiles } from "./readonly-files";

test("preloading waits for every file even if responses finish out of order", async () => {
  const done: string[] = [];
  const request: typeof fetch = async (url) => {
    const name = typeof url === "string" ? url : url instanceof URL ? url.href : url.url;
    await new Promise((r) => setTimeout(r, name.endsWith("a") ? 15 : 1));
    done.push(name);
    return new Response(name);
  };
  const files = await preloadFiles("/tree/", ["a", "b"], request);
  expect(done).toEqual(["/tree/b", "/tree/a"]);
  expect(new TextDecoder().decode(files.get("a"))).toBe("/tree/a");
});
test("a failed preload rejects rather than starting with missing files", async () => {
  await expect(
    preloadFiles("/tree/", ["missing"], async () => new Response(null, { status: 503 })),
  ).rejects.toThrow();
});
