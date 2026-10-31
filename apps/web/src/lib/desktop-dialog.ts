import { desktopRuntime } from "./backend";

interface FileFilter { name: string; extensions: string[] }
interface DialogOptions {
  title?: string;
  defaultPath?: string;
  filters?: FileFilter[];
}
interface OpenOptions extends DialogOptions { directory?: boolean; multiple?: boolean; recursive?: boolean }

const filters = (value?: FileFilter[]) => value?.map(({ name, extensions }) => ({
  DisplayName: name,
  Pattern: extensions.map((extension) => extension === "*" ? "*" : `*.${extension}`).join(";"),
}));

export async function open(options: OpenOptions & { multiple: true }): Promise<string[] | null>;
export async function open(options?: OpenOptions & { multiple?: false }): Promise<string | null>;
export async function open(options?: OpenOptions): Promise<string | string[] | null>;
export async function open(options: OpenOptions = {}): Promise<string | string[] | null> {
  if (desktopRuntime !== "wails") throw new Error("请在桌面应用中选择文件");
  const { Dialogs } = await import("@wailsio/runtime");
  const value = await Dialogs.OpenFile({
    Title: options.title, Directory: options.defaultPath,
    CanChooseDirectories: !!options.directory, CanChooseFiles: !options.directory,
    AllowsMultipleSelection: !!options.multiple, Filters: filters(options.filters),
  });
  return Array.isArray(value) ? value.length ? value : null : value || null;
}

export async function save(options: DialogOptions = {}): Promise<string | null> {
  if (desktopRuntime !== "wails") throw new Error("请在桌面应用中选择保存位置");
  const { Dialogs } = await import("@wailsio/runtime");
  const path = options.defaultPath ?? "";
  const separator = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return await Dialogs.SaveFile({
    Title: options.title, Filename: path.slice(separator + 1),
    Directory: separator < 0 ? undefined : path.slice(0, separator + 1),
    CanCreateDirectories: true, Filters: filters(options.filters),
  }) || null;
}
