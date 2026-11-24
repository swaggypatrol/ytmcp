import { describe, expect, it } from "vitest";
import {
  EnvironmentProvider,
  classifyDayPart,
  nextEnvironmentSampleDelayMs,
  resolveZonedBoundaryUtcMs,
} from "../provider.js";

const usAt = (utcMs: number, includeCulturalObservances: boolean) => {
  const provider = new EnvironmentProvider({ nowMs: () => utcMs, resolveTimeZone: () => "America/New_York" });
  provider.setConfig({
    configRevision: 1,
    holidaysEnabled: true,
    holidayCountry: "US",
    holidayState: null,
    holidayRegion: null,
    includeCulturalObservances,
  });
  return provider;
};
const labels = (provider: EnvironmentProvider) =>
  (provider.snapshot({ includeCountries: false }).occasions ?? []).map((o) => `${o.label} (${o.phase})`);

describe("EnvironmentProvider", () => {
  it("classifies local day parts and locks midnight across DST transitions", () => {
    expect(classifyDayPart(6).dayPart).toBe("earlyMorning");
    expect(classifyDayPart(10).dayPart).toBe("morning");
    expect(classifyDayPart(15).dayPart).toBe("afternoon");
    expect(classifyDayPart(19).dayPart).toBe("evening");
    expect(classifyDayPart(23).dayPart).toBe("lateNight");
    expect(classifyDayPart(2).dayPart).toBe("lateNight");

    // America/New_York DST spring-forward on 2026-03-08: 23-hour day (05:00Z -> 04:00Z)
    const springNow = Date.UTC(2026, 2, 8, 18, 0, 0); // 2026-03-08 14:00 EDT
    const springProvider = new EnvironmentProvider({
      nowMs: () => springNow,
      resolveTimeZone: () => "America/New_York",
    });
    const springSnap = springProvider.snapshot();
    expect(springSnap.clockOk).toBe(true);
    expect(springSnap.localDate).toBe("2026-03-08");
    expect(springSnap.dayPart).toBe("afternoon");
    // Midnight 2026-03-09 EDT is 04:00 UTC
    expect(springSnap.dayEndsAtUtcMs).toBe(Date.UTC(2026, 2, 9, 4, 0, 0));

    // Fall-back on 2026-11-01: 25-hour day (midnight 2026-11-02 EST is 05:00 UTC)
    const fallNow = Date.UTC(2026, 10, 1, 20, 0, 0);
    const fallProvider = new EnvironmentProvider({
      nowMs: () => fallNow,
      resolveTimeZone: () => "America/New_York",
    });
    const fallSnap = fallProvider.snapshot();
    expect(fallSnap.localDate).toBe("2026-11-01");
    expect(fallSnap.dayEndsAtUtcMs).toBe(Date.UTC(2026, 10, 2, 5, 0, 0));
    expect(resolveZonedBoundaryUtcMs(2026, 11, 2, 0, "America/New_York")).toBe(
      Date.UTC(2026, 10, 2, 5, 0, 0)
    );
  });

  it("reports offline public holidays, holiday eves, and optional cultural observances", () => {
    // 2026-12-24 afternoon in America/New_York -> Christmas Eve!
    const eveNow = Date.UTC(2026, 11, 24, 19, 0, 0);
    const provider = new EnvironmentProvider({
      nowMs: () => eveNow,
      resolveTimeZone: () => "America/New_York",
    });
    provider.setConfig({
      configRevision: 3,
      holidaysEnabled: true,
      holidayCountry: "US",
      holidayState: null,
      holidayRegion: null,
      includeCulturalObservances: false,
    });
    const snap = provider.snapshot();
    expect(snap.clockOk).toBe(true);
    expect(snap.calendarOk).toBe(true);
    expect(snap.occasions?.some((o) => o.phase === "eve" && o.label.includes("Christmas"))).toBe(true);
    expect(snap.countries?.some((c) => c.code === "US" && (c.states?.length ?? 0) > 0)).toBe(true);

    // Valentine's Day (observance) excluded unless includeCulturalObservances = true
    const valNow = Date.UTC(2026, 1, 14, 16, 0, 0);
    const valProvider = new EnvironmentProvider({
      nowMs: () => valNow,
      resolveTimeZone: () => "America/New_York",
    });
    valProvider.setConfig({
      configRevision: 1,
      holidaysEnabled: true,
      holidayCountry: "US",
      holidayState: null,
      holidayRegion: null,
      includeCulturalObservances: false,
    });
    expect(valProvider.snapshot().occasions).toEqual([]);
    valProvider.setConfig({
      configRevision: 2,
      holidaysEnabled: true,
      holidayCountry: "US",
      holidayState: null,
      holidayRegion: null,
      includeCulturalObservances: true,
    });
    expect(
      valProvider.snapshot().occasions?.some((o) => o.kind === "observance" && o.label.includes("Valentine"))
    ).toBe(true);
  });

  it("names an eve after the holiday itself, only for public holidays and only on an ordinary day", () => {
    const evening = (month: number, day: number) => Date.UTC(2026, month - 1, day, 23, 0, 0); // 18:00 in New York
    expect(labels(usAt(evening(11, 25), true))).toEqual(["Thanksgiving Day (eve)"]);
    expect(labels(usAt(evening(11, 26), true))).toEqual(["Thanksgiving Day (day)"]);
    expect(labels(usAt(evening(12, 23), true))).toEqual([]);
    expect(labels(usAt(evening(12, 24), true))).toEqual(["Christmas Eve (day)"]);
    expect(labels(usAt(evening(12, 24), false))).toEqual(["Christmas Day (eve)"]);
  });

  it("finds the local midnight even where DST skips it", () => {
    // Santiago springs forward from 00:00 to 01:00 on 2026-09-06; Havana on 2026-03-08.
    expect(new Date(resolveZonedBoundaryUtcMs(2026, 9, 6, 0, "America/Santiago")).toISOString()).toBe("2026-09-06T04:00:00.000Z");
    expect(new Date(resolveZonedBoundaryUtcMs(2026, 3, 8, 0, "America/Havana")).toISOString()).toBe("2026-03-08T05:00:00.000Z");
    expect(new Date(resolveZonedBoundaryUtcMs(2026, 11, 2, 0, "America/New_York")).toISOString()).toBe("2026-11-02T05:00:00.000Z");
  });

  it("always samples again before the snapshot it sent runs out, through day parts and midnight", () => {
    let now = Date.UTC(2026, 10, 24, 4, 30, 0); // 23:30 in New York
    const end = now + 26 * 3_600_000;
    let sent = new EnvironmentProvider({ nowMs: () => now, resolveTimeZone: () => "America/New_York" }).snapshot({ includeCountries: false });
    while (now < end) {
      now += nextEnvironmentSampleDelayMs(sent, now, 60_000);
      // At most the timer slack after a boundary passes without a fresh snapshot.
      expect(now - sent.validUntilMs).toBeLessThanOrEqual(50);
      sent = new EnvironmentProvider({ nowMs: () => now, resolveTimeZone: () => "America/New_York" }).snapshot({ includeCountries: false });
      expect(sent.validUntilMs).toBeGreaterThan(now);
    }
  });

  it("isolates calendar failures so device clock remains valid", () => {
    const provider = new EnvironmentProvider({
      nowMs: () => Date.UTC(2026, 5, 15, 14, 0, 0),
      resolveTimeZone: () => "UTC",
    });
    provider.setConfig({
      configRevision: 4,
      holidaysEnabled: true,
      holidayCountry: "INVALID_XX",
      holidayState: null,
      holidayRegion: null,
      includeCulturalObservances: false,
    });
    const snap = provider.snapshot();
    expect(snap.clockOk).toBe(true);
    expect(snap.localDate).toBe("2026-06-15");
    expect(snap.calendarOk).toBe(false);
    expect(snap.occasions).toEqual([]);
  });
});
