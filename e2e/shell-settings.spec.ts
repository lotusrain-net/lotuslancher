import { expect, test } from "@playwright/test";

test("the desktop settings drawer can change the control server", async ({ page }) => {
  await page.goto("/shell/index.html#drawer");

  const address = page.getByRole("textbox", { name: "服务端地址" });
  const save = page.getByRole("button", { name: "保存服务端地址" });
  await expect(address).toHaveValue("https://hub.lotusrain.net");
  await expect(address).toBeEnabled();

  await address.fill("http://example.test:8080");
  await expect(page.getByText("请输入 HTTPS 地址，或本机回环 HTTP 地址。", { exact: true })).toBeVisible();
  await expect(save).toBeDisabled();

  await address.fill("http://127.0.0.1:8080");
  await expect(save).toBeEnabled();
  await save.click();
  await expect(page.getByText("服务端地址已保存，重启客户端后生效", { exact: true })).toBeVisible();
  await expect(address).toHaveValue("http://127.0.0.1:8080");
});
