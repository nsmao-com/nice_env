import { desktopRuntime } from "./backend";

const wailsWindow = async () => (await import("@wailsio/runtime")).Window;

// Keep runtime imports lazy: loading Wails in a browser would install native
// input handlers and could make a preview appear to be a desktop application.
export function getCurrentWindow() {
  return {
    async isMaximized() {
      if (desktopRuntime === "wails") return (await wailsWindow()).IsMaximised();
      return false;
    },
    async minimize() {
      if (desktopRuntime === "wails") return (await wailsWindow()).Minimise();
    },
    async toggleMaximize() {
      if (desktopRuntime === "wails") return (await wailsWindow()).ToggleMaximise();
    },
    async close() {
      if (desktopRuntime === "wails") return (await wailsWindow()).Close();
    },
    async onResized(handler: () => void): Promise<() => void> {
      window.addEventListener("resize", handler);
      return () => window.removeEventListener("resize", handler);
    },
  };
}
