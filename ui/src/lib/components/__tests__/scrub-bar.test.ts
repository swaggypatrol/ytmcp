import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import ScrubBar from "../ScrubBar.svelte";

afterEach(cleanup);

const host = (input: HTMLElement) => input.closest(".scrub") as HTMLElement;

describe("ScrubBar", () => {
  it("fills the bar to the value, has no thumb to draw, and stays a real, labelled range input", async () => {
    const oninput = vi.fn();
    render(ScrubBar, { props: { value: 30, max: 120, label: "Seek", valueText: "0:30 / 2:00", oninput, testid: "s" } });
    const input = screen.getByTestId("s") as HTMLInputElement;
    expect(input.type).toBe("range");
    expect(input.getAttribute("aria-label")).toBe("Seek");
    expect(input.getAttribute("aria-valuetext")).toBe("0:30 / 2:00");
    expect(host(input).style.getPropertyValue("--fill")).toBe("25.000%");
    expect(host(input).querySelectorAll(".scrub-fill")).toHaveLength(1);
    // Nothing loaded is known: no buffered layer width.
    expect(host(input).style.getPropertyValue("--buffered")).toBe("0.000%");
    await fireEvent.input(input, { target: { value: "60" } });
    expect(oninput).toHaveBeenLastCalledWith(60);
  });

  it("shows how far the media has loaded behind the played part", () => {
    render(ScrubBar, { props: { value: 3_000, max: 240_000, buffered: 68_000, label: "Seek", testid: "s" } });
    const input = screen.getByTestId("s") as HTMLInputElement;
    expect(host(input).style.getPropertyValue("--fill")).toBe("1.250%");
    expect(host(input).style.getPropertyValue("--buffered")).toBe("28.333%");
    expect(host(input).querySelectorAll(".scrub-buffered")).toHaveLength(1);
  });

  it("is marked pressed while held and settles back on release", async () => {
    const onpress = vi.fn();
    const onrelease = vi.fn();
    render(ScrubBar, { props: { value: 50, max: 100, label: "Volume", onpress, onrelease, testid: "s" } });
    const input = screen.getByTestId("s") as HTMLInputElement;

    expect(host(input).classList.contains("is-pressed")).toBe(false);
    await fireEvent.pointerDown(input, { clientX: 250 });
    expect(host(input).classList.contains("is-pressed")).toBe(true);
    expect(onpress).toHaveBeenCalledTimes(1);

    await fireEvent.pointerUp(input);
    expect(host(input).classList.contains("is-pressed")).toBe(false);
    expect(onrelease).toHaveBeenCalledTimes(1);
  });

  it("does not react while disabled", async () => {
    render(ScrubBar, { props: { value: 10, max: 100, label: "Seek", disabled: true, testid: "s" } });
    const input = screen.getByTestId("s") as HTMLInputElement;
    await fireEvent.pointerDown(input, { clientX: 0 });
    expect(host(input).classList.contains("is-pressed")).toBe(false);
  });
});
