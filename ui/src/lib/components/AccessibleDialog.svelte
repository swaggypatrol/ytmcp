<script lang="ts" module>
  let dialogCounter = 0;
</script>

<script lang="ts">
  import { untrack, type Snippet } from "svelte";
  import { glass } from "../glass/liquid-glass.js";
  import { OverlayPresence } from "../glass/overlay-motion.svelte.js";
  import { t } from "../i18n/index.svelte.js";

  interface Props {
    open: boolean;
    title: string;
    onClose: () => void;
    children: Snippet;
  }

  let { open, title, onClose, children }: Props = $props();

  let dialogEl = $state<HTMLDivElement | null>(null);
  let previousFocused: HTMLElement | null = null;
  const headingId = `ytm-dialog-title-${++dialogCounter}`;
  let layerEl = $state<HTMLDivElement | null>(null);
  const presence = new OverlayPresence();
  $effect(() => () => presence.dispose());

  function getFocusableElements(): HTMLElement[] {
    if (!dialogEl) return [];
    const selector =
      'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';
    return Array.from(dialogEl.querySelectorAll<HTMLElement>(selector));
  }

  /** True when the element or an ancestor is inert, where focus() silently does nothing. */
  function isInert(el: HTMLElement): boolean {
    for (let node: HTMLElement | null = el; node; node = node.parentElement) if (node.inert) return true;
    return false;
  }

  // Move focus into the dialog on open and return it to the opener on close.
  $effect(() => {
    if (open) {
      untrack(() => presence.show());
      previousFocused = document.activeElement as HTMLElement | null;
      queueMicrotask(() => {
        if (!open) return;
        const focusables = getFocusableElements();
        (focusables[0] ?? dialogEl)?.focus();
      });
    } else {
      untrack(() => presence.hide(layerEl, dialogEl));
      if (!previousFocused) return;
      const target = previousFocused;
      previousFocused = null;
      queueMicrotask(() => {
        if (open) return;
        // The opener may be gone or inert (a toast that left): fall back to the page title, then main.
        if (target.isConnected && !isInert(target)) target.focus();
        if (document.activeElement === target) return;
        const fallback =
          document.querySelector<HTMLElement>(".app-page h1") ?? document.querySelector<HTMLElement>(".app-scroll");
        if (!fallback) return;
        if (!fallback.hasAttribute("tabindex")) fallback.setAttribute("tabindex", "-1");
        fallback.focus({ preventScroll: true });
      });
    }
  });

  /**
   * Moves the dialog layer to the end of the app shell, so a dialog opened from inside a page
   * (a stacking context of its own) still covers the glass on L5 and still reads the shell's
   * metrics. Outside the shell it stays where it is.
   */
  function toShell(node: HTMLElement) {
    const shell = node.parentElement?.closest(".app-shell");
    if (!shell) return;
    shell.append(node);
    return () => node.remove();
  }

  function handleKeydown(e: KeyboardEvent) {
    if (!open) return;
    // Keep keys inside the dialog: global shortcuts must not fire behind a modal.
    e.stopPropagation();
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
      return;
    }
    if (e.key === "Tab") {
      const focusables = getFocusableElements();
      if (focusables.length === 0) {
        e.preventDefault();
        return;
      }
      const first = focusables[0]!;
      const last = focusables[focusables.length - 1]!;
      const active = document.activeElement;
      if (e.shiftKey && (active === first || active === dialogEl)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    }
  }
</script>

{#if presence.rendered}
  <!-- The wrapper stays in place; the layer itself moves to the shell (see toShell). -->
  <div class="contents">
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <!-- The scrim is a plain dim, never a blur; the panel is the only glass in the layer. -->
    <div
      bind:this={layerEl}
      class="app-dialog-layer"
      role="presentation"
      onclick={(e) => {
        if (open && e.target === e.currentTarget) onClose();
      }}
      onkeydown={handleKeydown}
      {@attach toShell}
    >
      <div
        bind:this={dialogEl}
        role="dialog"
        aria-modal="true"
        aria-labelledby={headingId}
        tabindex="-1"
        class="lg lg--popover lg-overlay surface-rounded-xl w-full max-w-2xl max-h-full flex flex-col overflow-hidden"
        data-testid="accessible-dialog"
        {@attach glass}
      >
        <header class="lg-overlay-content flex items-center justify-between gap-4 px-5 pt-4 pb-2">
          <h2 id={headingId} class="type-lg font-semibold m-0">{title}</h2>
          <button type="button" class="btn-secondary lg-press" onclick={onClose} data-testid="dialog-close-btn">
            {t.dialog.close}
          </button>
        </header>
        <!-- The scroll area stops short of the lower rim, so clipped text never meets the highlight;
             6px above the first child keeps its focus ring inside the scroll clip. -->
        <div class="lg-overlay-content px-5 pt-1.5 pb-2 mb-3 overflow-y-auto flex-1">
          {@render children()}
        </div>
      </div>
    </div>
  </div>
{/if}
