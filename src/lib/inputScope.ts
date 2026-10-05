/**
 * 明细页 Tokens 列的输入口径偏好：输入数字是否并入缓存读取。
 * 纯前端显示偏好（归属同明暗/强调色），Rust 侧只在 paths.rs 存键名做镜像。
 */

import { LS_KEY_INPUT_SCOPE } from "../constants";

export const INPUT_SCOPES = ["input", "inputWithCacheRead"] as const;

/** 输入口径：`input` 只计原始输入桶；`inputWithCacheRead` 把缓存读并入输入显示。 */
export type InputScope = (typeof INPUT_SCOPES)[number];

export const DEFAULT_INPUT_SCOPE: InputScope = "input";

function parseInputScope(value: string | null): InputScope | null {
  return (INPUT_SCOPES as readonly string[]).includes(value ?? "")
    ? (value as InputScope)
    : null;
}

function readStored(
  key: string,
  storage: Pick<Storage, "getItem">,
): string | null {
  try {
    return storage.getItem(key);
  } catch {
    // 静默跳过：读不到就当没存过。
    return null;
  }
}

function writeStored(
  key: string,
  value: string,
  storage: Pick<Storage, "setItem">,
): void {
  try {
    storage.setItem(key, value);
  } catch {
    // 静默跳过：写不进去也不该影响本次会话。
  }
}

export function readStoredInputScope(
  storage: Pick<Storage, "getItem"> = localStorage,
): InputScope | null {
  return parseInputScope(readStored(LS_KEY_INPUT_SCOPE, storage));
}

export function storeInputScope(
  scope: InputScope,
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  writeStored(LS_KEY_INPUT_SCOPE, scope, storage);
}

/** 读出用户存的口径；没存过或存了不认识的值就是仅输入。 */
export function readInputScopeSettings(
  storage: Pick<Storage, "getItem"> = localStorage,
): InputScope {
  return readStoredInputScope(storage) ?? DEFAULT_INPUT_SCOPE;
}
