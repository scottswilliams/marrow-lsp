import * as path from "path";

// Binary resolution for the launched transports, isolated from the vscode and fs
// modules so it can be exercised directly. The extension supplies the host
// (configured setting value, extension path, run mode, and a file-existence probe);
// this module owns only the resolution order.

export type LauncherMode = "production" | "development";

export interface LauncherHost {
  /** The user-configured absolute path for `marrow.<setting>`, if any. */
  configuredPath(setting: string): string | undefined;
  /** The installed extension directory. */
  readonly extensionPath: string;
  /** Production for a shipped install; development for an extension dev host. */
  readonly mode: LauncherMode;
  /** Whether the candidate path is an existing regular file. */
  isFile(candidate: string): boolean;
}

// A configured path only counts when it is absolute and points at a real file; a
// relative or missing path is treated as unset (undefined), not a hard error.
function usableBinary(host: LauncherHost, file: string): boolean {
  if (!path.isAbsolute(file)) {
    return false;
  }
  return host.isFile(file);
}

// Resolution order: an explicit configured path wins (and if set-but-unusable,
// resolves to nothing rather than falling back to a binary the user did not choose);
// otherwise the bundled binary; otherwise, only outside a production install, the
// dev build. Returns undefined when nothing usable is found.
export function resolveBinaryPath(
  host: LauncherHost,
  setting: string,
  binary: string,
): string | undefined {
  const configured = host.configuredPath(setting)?.trim();
  if (configured) {
    return usableBinary(host, configured) ? configured : undefined;
  }

  const bundled = path.join(host.extensionPath, "server", binary);
  if (usableBinary(host, bundled)) {
    return bundled;
  }

  const dev = path.join(host.extensionPath, "..", "..", "target", "debug", binary);
  if (host.mode !== "production" && usableBinary(host, dev)) {
    return dev;
  }

  return undefined;
}
