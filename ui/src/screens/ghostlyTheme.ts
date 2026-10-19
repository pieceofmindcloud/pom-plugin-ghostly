/**
 * Ghostly in the POM's colours, without touching Ghostly's code.
 *
 * Ghostly paints from `data-theme` (dark/light) and `data-color-theme` on its
 * root element, through `--theme-*` variables. The app is served on the POM's
 * own origin, so the screen reaches into its frame: it sets the POM's scheme,
 * keeps Ghostly's ice-blue theme (the nearest to the POM's blue and teal) and
 * lays the POM's surfaces, lines, text and teal accent over it. Whatever is not
 * redefined here (danger, links, member colours) stays Ghostly's own.
 */

export type PomTheme = "light" | "dark";

const MARK = "data-pom-theme";
const STYLE_ID = "pom-theme";
const COLOR_THEME = "cyan";

/** The POM's tokens (pom-jair `apps/frontend/src/index.css`) in Ghostly's names. */
const PALETTE: Record<PomTheme, Record<string, string>> = {
  dark: {
    "app-bg": "#12141c",
    "sidebar-bg": "#090a10",
    "chat-bg": "#090a10",
    "panel-header": "#12141c",
    surface: "#12141c",
    "surface-alt": "#191c27",
    "surface-hover": "#262a38",
    border: "#262a38",
    "border-bright": "#373d50",
    "text-primary": "#e8eaf2",
    "text-secondary": "#b4b9cb",
    "text-muted": "#8a90a6",
    "received-bg": "#191c27",
    "input-bg": "#191c27",
    "search-bg": "#12141c",
    "chat-pattern": "#0e1017",
    scrollbar: "#262a38",
    "scrollbar-hover": "#373d50",
    accent: "#43e0c8",
    "accent-dim": "#124f47",
    "accent-hover": "#5befd8",
    "sent-bg": "#12443e",
    "on-accent": "#090a10",
  },
  light: {
    "app-bg": "#f7f8fb",
    "sidebar-bg": "#ffffff",
    "chat-bg": "#f7f8fb",
    "panel-header": "#ffffff",
    surface: "#ffffff",
    "surface-alt": "#fafbfe",
    "surface-hover": "#eef1f7",
    border: "#e2e6f0",
    "border-bright": "#cdd3e2",
    "text-primary": "#12141e",
    "text-secondary": "#3f465c",
    "text-muted": "#5f677e",
    "received-bg": "#ffffff",
    "input-bg": "#f2f4f9",
    "search-bg": "#f2f4f9",
    "chat-pattern": "#eceff6",
    scrollbar: "#cdd3e2",
    "scrollbar-hover": "#b3bbd0",
    accent: "#0b8279",
    "accent-dim": "#d6f3ef",
    "accent-hover": "#08665f",
    "sent-bg": "#d6f3ef",
    "on-accent": "#ffffff",
  },
};

/** The stylesheet laid over Ghostly's: one rule per scheme, above its own. */
export function pomThemeCss(): string {
  return (Object.keys(PALETTE) as PomTheme[])
    .map((scheme) => {
      const body = Object.entries(PALETTE[scheme])
        .map(([name, value]) => `  --theme-${name}: ${value};`)
        .join("\n");
      return `:root[data-theme="${scheme}"][${MARK}] {\n${body}\n}`;
    })
    .join("\n");
}

/** Puts the frame's document in `theme`; true when anything changed. */
export function applyPomTheme(root: HTMLElement, theme: PomTheme): boolean {
  const document = root.ownerDocument;
  let changed = false;
  if (!document.getElementById(STYLE_ID)) {
    const style = document.createElement("style");
    style.id = STYLE_ID;
    style.textContent = pomThemeCss();
    document.head.append(style);
    changed = true;
  }
  const wanted: Record<string, string> = { "data-theme": theme, "data-color-theme": COLOR_THEME, [MARK]: "" };
  for (const [name, value] of Object.entries(wanted)) {
    if (root.getAttribute(name) !== value) {
      root.setAttribute(name, value);
      changed = true;
    }
  }
  if (root.style.colorScheme !== theme) {
    root.style.colorScheme = theme;
    changed = true;
  }
  return changed;
}

/**
 * Keeps the frame's document in `theme`: now, and again whenever Ghostly
 * writes its own scheme back (on start, or from its Settings). Returns the
 * function that stops watching.
 */
export function holdPomTheme(frame: HTMLIFrameElement, theme: PomTheme): () => void {
  let root: HTMLElement | null = null;
  let watcher: MutationObserver | null = null;
  const attach = () => {
    watcher?.disconnect();
    try {
      root = frame.contentDocument?.documentElement ?? null;
    } catch {
      root = null;
    }
    if (!root) return;
    applyPomTheme(root, theme);
    watcher = new MutationObserver(() => root && applyPomTheme(root, theme));
    watcher.observe(root, { attributes: true, attributeFilter: ["data-theme", "data-color-theme", MARK, "style"] });
  };
  attach();
  frame.addEventListener("load", attach);
  return () => {
    frame.removeEventListener("load", attach);
    watcher?.disconnect();
  };
}
