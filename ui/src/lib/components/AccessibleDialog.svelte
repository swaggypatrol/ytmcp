<script lang="ts" module>
  let dialogCounter = 0;
</script>

<script lang="ts">
  import type { Snippet } from "svelte";
  import { glass } from "../glass/liquid-glass.js";
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

  function getFocusableElements(): HTMLElement[] {
    if (!dialogEl) return [];
    const selector =
      'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';
    return Array.from(dialogEl.querySelectorAll<HTMLElement>(selector));
  }

  // Move focus into the dialog on open and return it to the opener on close.
  $effect(() => {
    if (open) {
      previousFocused = document.activeElement as HTMLElement | null;
      queueMicrotask(() => {
        const focusables = getFocusableElements();
        (focusables[0] ?? dialogEl)?.focus();
      });
    } else if (previousFocused) {
      const target = previousFocused;
      previousFocused = null;
      queueMicrotask(() => {
        if (target.isConnected) target.focus();
      });
    }
  });

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

{#if open}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- The scrim is a plain dim, never a blur; the panel is the only glass in the layer. -->
  <div
    class="app-dialog-layer"
    role="presentation"
    onclick={(e) => {
      if (e.target === e.currentTarget) onClose();
    }}
    onkeydown={handleKeydown}
  >
    <div
      bind:this={dialogEl}
      role="dialog"
      aria-modal="true"
      aria-labelledby={headingId}
      tabindex="-1"
      class="lg lg--popover lg-morph surface-rounded-xl w-full max-w-2xl max-h-full flex flex-col overflow-hidden"
      data-testid="accessible-dialog"
      {@attach glass}
    >
      <header class="flex items-center justify-between gap-4 px-5 pt-4 pb-2">
        <h2 id={headingId} class="type-lg font-semibold m-0">{title}</h2>
        <button type="button" class="btn-secondary lg-press" onclick={onClose} data-testid="dialog-close-btn">
          {t.dialog.close}
        </button>
      </header>
      <!-- The scroll area stops short of the lower rim, so clipped text never meets the highlight;
           6px above the first child keeps its focus ring inside the scroll clip. -->
      <div class="px-5 pt-1.5 pb-2 mb-3 overflow-y-auto flex-1">
        {@render children()}
      </div>
    </div>
  </div>
{/if}
