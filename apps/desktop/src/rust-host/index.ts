export {
  RustHostClient,
  resolveDaemonBinaryPath,
  type DaemonStatus,
  type MprisUiAction,
  type RustHostClientOptions,
} from "./client.js";
export {
  attachPlayerBridgeInjector,
  buildBridgeInjectionScript,
  injectBridgeIntoMusicPlayer,
  isTrustedPlayerMusicFrame,
  loadBridgeBundleAssets,
  resolveBridgeDistDir,
  type BridgeBundleAssets,
} from "./bridge-injector.js";
export {
  createIntegratedDesktopHooks,
  resolveCoversCacheRoot,
  type CreateIntegratedDesktopHooksOptions,
  type IntegratedDesktopHost,
} from "./desktop-hooks.js";
