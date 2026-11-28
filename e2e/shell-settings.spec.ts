import { expect, test } from "@playwright/test";

test("the desktop settings drawer validates and normalizes the control server", async ({ page }) => {
  await page.goto("/shell/index.html#drawer");

  const address = page.getByRole("textbox", { name: "服务端地址" });
  const save = page.getByRole("button", { name: "保存服务端地址" });
  await expect(address).toHaveValue("https://hub.lotusrain.net");
  await expect(address).toBeEnabled();

  await address.fill("http://example.test:8080");
  await expect(page.getByText("请输入 HTTPS 地址，或本机回环 HTTP 地址。", { exact: true })).toBeVisible();
  await expect(save).toBeDisabled();

  await address.fill("http://127.0.0.1:8080/");
  await expect(save).toBeEnabled();
  await save.click();
  await expect(page.getByText("服务端地址已保存，重启客户端后生效", { exact: true })).toBeVisible();
  await expect(address).toHaveValue("http://127.0.0.1:8080");
});

test("preference refreshes preserve an unsaved control server draft", async ({ page }) => {
  await page.goto("/shell/index.html#drawer");

  const address = page.getByRole("textbox", { name: "服务端地址" });
  const draft = "https://draft-control.example";
  await expect(address).toBeEnabled();
  await address.fill(draft);

  await page.getByRole("switch", { name: "开机自动启动" }).click();
  await expect(address).toHaveValue(draft);
  await page.getByRole("switch", { name: "连接 LotusHub 时使用系统代理" }).click();
  await expect(address).toHaveValue(draft);
});

test("the control server section is hidden when editing is disabled", async ({ page }) => {
  await page.goto("/shell/index.html?control_editable=0#drawer");
  await expect(page.getByRole("heading", { name: "服务端连接" })).toHaveCount(0);
});

test("the control server section is read-only when pinned by build override", async ({ page }) => {
  await page.goto("/shell/index.html?control_overridden=1#drawer");
  const address = page.getByRole("textbox", { name: "服务端地址" });
  const save = page.getByRole("button", { name: "保存服务端地址" });
  await expect(address).toHaveValue("http://127.0.0.1:18080");
  await expect(address).toBeDisabled();
  await expect(save).toBeDisabled();
  await expect(page.getByText("服务端地址由构建配置固定，无法在此修改。", { exact: true })).toBeVisible();
});
