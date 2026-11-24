import Holidays, { type HolidaysTypes } from "date-holidays";
import type {
  EnvironmentConfigPayload,
  HolidayCountryOption,
  HolidaySubdivisionOption,
  HostEnvironmentSnapshotPayload,
  HostOccasion,
} from "@ytmcp/protocol";

export const ENVIRONMENT_PROVIDER_VERSION = "date-holidays@3.39.2";
const MAX_VALIDITY_MS = 5 * 60_000;

interface ZonedLocalParts {
  year: number;
  month: number;
  day: number;
  hour: number;
  minute: number;
  second: number;
  weekday: string;
  localDate: string;
}

export interface EnvironmentProviderOptions {
  nowMs?: () => number;
  resolveTimeZone?: () => string;
}

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

function addCalendarDays(year: number, month: number, day: number, deltaDays: number): {
  year: number;
  month: number;
  day: number;
  localDate: string;
} {
  const dt = new Date(Date.UTC(year, month - 1, day + deltaDays));
  const y = dt.getUTCFullYear();
  const m = dt.getUTCMonth() + 1;
  const d = dt.getUTCDate();
  return {
    year: y,
    month: m,
    day: d,
    localDate: `${y}-${pad2(m)}-${pad2(d)}`,
  };
}

function zonedPartsAt(utcMs: number, timeZone: string): ZonedLocalParts {
  const dtf = new Intl.DateTimeFormat("en-US", {
    timeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    weekday: "long",
    hourCycle: "h23",
  });
  const parts = dtf.formatToParts(new Date(utcMs));
  const get = (type: Intl.DateTimeFormatPartTypes): string =>
    parts.find((p) => p.type === type)?.value ?? "";
  const year = Number.parseInt(get("year"), 10);
  const month = Number.parseInt(get("month"), 10);
  const day = Number.parseInt(get("day"), 10);
  let hour = Number.parseInt(get("hour"), 10);
  if (hour === 24) hour = 0;
  const minute = Number.parseInt(get("minute"), 10);
  const second = Number.parseInt(get("second"), 10);
  const weekday = get("weekday").toLowerCase();
  if (
    !Number.isFinite(year) ||
    !Number.isFinite(month) ||
    !Number.isFinite(day) ||
    !Number.isFinite(hour) ||
    !weekday
  ) {
    throw new Error(`Invalid local clock parts for timezone ${timeZone}`);
  }
  return {
    year,
    month,
    day,
    hour,
    minute,
    second,
    weekday,
    localDate: `${year}-${pad2(month)}-${pad2(day)}`,
  };
}

export function classifyDayPart(hour: number): {
  dayPart: "earlyMorning" | "morning" | "afternoon" | "evening" | "lateNight";
  nextBoundaryHour: number;
  boundaryNextDay: boolean;
} {
  if (hour >= 5 && hour < 8) {
    return { dayPart: "earlyMorning", nextBoundaryHour: 8, boundaryNextDay: false };
  }
  if (hour >= 8 && hour < 12) {
    return { dayPart: "morning", nextBoundaryHour: 12, boundaryNextDay: false };
  }
  if (hour >= 12 && hour < 17) {
    return { dayPart: "afternoon", nextBoundaryHour: 17, boundaryNextDay: false };
  }
  if (hour >= 17 && hour < 22) {
    return { dayPart: "evening", nextBoundaryHour: 22, boundaryNextDay: false };
  }
  if (hour >= 22) {
    return { dayPart: "lateNight", nextBoundaryHour: 5, boundaryNextDay: true };
  }
  return { dayPart: "lateNight", nextBoundaryHour: 5, boundaryNextDay: false };
}

/**
 * The first UTC instant at which the clock in `timeZone` reads `hour`:00 on the given date or
 * later. Found by bisecting a window wide enough for every offset, so it stays right when DST
 * skips or repeats that hour (midnight included).
 */
export function resolveZonedBoundaryUtcMs(
  year: number,
  month: number,
  day: number,
  hour: number,
  timeZone: string
): number {
  const target = `${year}-${pad2(month)}-${pad2(day)}T${pad2(hour)}`;
  const reached = (minute: number): boolean => {
    const p = zonedPartsAt(minute * 60_000, timeZone);
    return `${p.localDate}T${pad2(p.hour)}` >= target;
  };
  const fakeUtcMinute = Date.UTC(year, month - 1, day, hour) / 60_000;
  let before = fakeUtcMinute - 26 * 60;
  let after = fakeUtcMinute + 26 * 60;
  while (after - before > 1) {
    const mid = Math.floor((before + after) / 2);
    if (reached(mid)) after = mid;
    else before = mid;
  }
  return after * 60_000;
}

/**
 * How long the host waits before sampling again: shortly before the snapshot expires when it
 * lasts a while, or just after an imminent day-part or day boundary, so a valid snapshot is
 * always in place. Never longer than `maxMs`, which also bounds how late a clock change is seen.
 */
export function nextEnvironmentSampleDelayMs(
  snapshot: Pick<HostEnvironmentSnapshotPayload, "validUntilMs">,
  nowMs: number,
  maxMs: number
): number {
  const left = snapshot.validUntilMs - nowMs;
  return left > 30_000 ? Math.min(maxMs, left - 20_000) : Math.max(1_000, left + 50);
}

function slugify(label: string): string {
  return label
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64) || "holiday";
}

function mapHolidayKind(type: string): "public" | "observance" {
  return type === "public" || type === "bank" ? "public" : "observance";
}

export class EnvironmentProvider {
  private readonly nowMs: () => number;
  private readonly resolveTimeZone: () => string;
  private config: EnvironmentConfigPayload = {
    configRevision: 0,
    holidaysEnabled: false,
    holidayCountry: null,
    holidayState: null,
    holidayRegion: null,
    includeCulturalObservances: false,
  };
  private readonly yearCache = new Map<string, HolidaysTypes.Holiday[]>();
  private countryCatalogCache: {
    selectedCountry: string | null;
    options: HolidayCountryOption[];
  } | null = null;

  constructor(options: EnvironmentProviderOptions = {}) {
    this.nowMs = options.nowMs ?? (() => Date.now());
    this.resolveTimeZone =
      options.resolveTimeZone ?? (() => Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC");
  }

  public setConfig(next: EnvironmentConfigPayload): void {
    if (next.holidayCountry !== this.config.holidayCountry) {
      this.countryCatalogCache = null;
    }
    this.config = next;
  }

  public getConfig(): EnvironmentConfigPayload {
    return this.config;
  }

  public snapshot(options?: { includeCountries?: boolean }): HostEnvironmentSnapshotPayload {
    const capturedAtMs = this.nowMs();
    const includeCountries = options?.includeCountries ?? true;

    // 1. Local clock (isolated from holiday calendar logic)
    let clockOk = false;
    let timeZone: string | null = null;
    let localDate: string | null = null;
    let weekday: string | null = null;
    let dayPart: "earlyMorning" | "morning" | "afternoon" | "evening" | "lateNight" | null = null;
    let dayEndsAtUtcMs: number | null = null;
    let validUntilMs = capturedAtMs + MAX_VALIDITY_MS;
    let parts: ZonedLocalParts | null = null;

    try {
      timeZone = this.resolveTimeZone();
      parts = zonedPartsAt(capturedAtMs, timeZone);
      localDate = parts.localDate;
      weekday = parts.weekday;
      const dp = classifyDayPart(parts.hour);
      dayPart = dp.dayPart;

      const tomorrow = addCalendarDays(parts.year, parts.month, parts.day, 1);
      dayEndsAtUtcMs = resolveZonedBoundaryUtcMs(tomorrow.year, tomorrow.month, tomorrow.day, 0, timeZone);

      const boundaryDay = dp.boundaryNextDay
        ? tomorrow
        : { year: parts.year, month: parts.month, day: parts.day };
      const nextBoundaryUtcMs = resolveZonedBoundaryUtcMs(
        boundaryDay.year,
        boundaryDay.month,
        boundaryDay.day,
        dp.nextBoundaryHour,
        timeZone
      );
      validUntilMs = Math.min(
        dayEndsAtUtcMs,
        nextBoundaryUtcMs > capturedAtMs ? nextBoundaryUtcMs : capturedAtMs + MAX_VALIDITY_MS,
        capturedAtMs + MAX_VALIDITY_MS
      );
      if (validUntilMs <= capturedAtMs) {
        validUntilMs = capturedAtMs + 60_000;
      }
      clockOk = true;
    } catch {
      clockOk = false;
    }

    // 2. Country catalog + offline calendar occasions (isolated from clock calculation)
    let countries: HolidayCountryOption[] = [];
    let calendarOk = true;
    let occasions: HostOccasion[] = [];

    if (includeCountries) {
      try {
        countries = this.getCountries(this.config.holidayCountry ?? null);
      } catch {
        countries = [];
      }
    }

    if (this.config.holidaysEnabled && this.config.holidayCountry) {
      if (!parts || !localDate) {
        calendarOk = false;
      } else {
        try {
          const tomorrow = addCalendarDays(parts.year, parts.month, parts.day, 1);
          occasions = this.lookupOccasions(
            this.config.holidayCountry,
            this.config.holidayState ?? null,
            this.config.holidayRegion ?? null,
            parts.year,
            tomorrow.year,
            localDate,
            tomorrow.localDate,
            this.config.includeCulturalObservances
          );
          calendarOk = true;
        } catch {
          calendarOk = false;
          occasions = [];
        }
      }
    }

    return {
      configRevision: this.config.configRevision,
      capturedAtMs,
      validUntilMs,
      clockOk,
      calendarOk,
      timeZone,
      localDate,
      weekday,
      dayPart,
      dayEndsAtUtcMs,
      occasions,
      countries,
    };
  }

  private getCountries(selectedCountry: string | null): HolidayCountryOption[] {
    if (this.countryCatalogCache && this.countryCatalogCache.selectedCountry === selectedCountry) {
      return this.countryCatalogCache.options;
    }
    const base = new Holidays({ languages: "en" });
    const rawCountries = base.getCountries("en") ?? {};
    const options: HolidayCountryOption[] = Object.entries(rawCountries)
      .map(([code, name]) => {
        let states: HolidaySubdivisionOption[] = [];
        if (selectedCountry && code === selectedCountry) {
          const rawStates = base.getStates(code, "en") ?? {};
          states = Object.entries(rawStates)
            .map(([sCode, sName]) => ({ code: sCode, name: sName }))
            .sort((a, b) => a.name.localeCompare(b.name));
        }
        return { code, name, states };
      })
      .sort((a, b) => a.name.localeCompare(b.name));
    this.countryCatalogCache = { selectedCountry, options };
    return options;
  }

  private getYearHolidays(
    country: string,
    state: string | null,
    region: string | null,
    year: number
  ): HolidaysTypes.Holiday[] {
    const cacheKey = `${country}|${state ?? ""}|${region ?? ""}|${year}`;
    const cached = this.yearCache.get(cacheKey);
    if (cached) {
      return cached;
    }

    const base = new Holidays({ languages: "en" });
    const supported = base.getCountries("en") ?? {};
    if (!Object.prototype.hasOwnProperty.call(supported, country)) {
      throw new Error(`Unsupported holiday country code: ${country}`);
    }

    const hd = region
      ? new Holidays(country, state ?? "", region, { languages: "en" })
      : state
        ? new Holidays(country, state, { languages: "en" })
        : new Holidays(country, { languages: "en" });

    const list = hd.getHolidays(year) ?? [];
    if (this.yearCache.size > 16) {
      this.yearCache.clear();
    }
    this.yearCache.set(cacheKey, list);
    return list;
  }

  private lookupOccasions(
    country: string,
    state: string | null,
    region: string | null,
    todayYear: number,
    tomorrowYear: number,
    todayDate: string,
    tomorrowDate: string,
    includeObservances: boolean
  ): HostOccasion[] {
    const countryLower = country.toLowerCase();
    const years = todayYear === tomorrowYear ? [todayYear] : [todayYear, tomorrowYear];
    const all = years.flatMap((y) => this.getYearHolidays(country, state, region, y));

    const results: HostOccasion[] = [];
    const seenIds = new Set<string>();

    for (const h of all) {
      const hDate = h.date.slice(0, 10);
      if (hDate !== todayDate) continue;
      const kind = mapHolidayKind(h.type);
      if (!includeObservances && kind === "observance") continue;
      const id = `occasion:${countryLower}:${slugify(h.name)}`;
      if (seenIds.has(id)) continue;
      seenIds.add(id);
      results.push({
        id,
        label: h.name,
        phase: h.substitute ? "observed" : "day",
        kind,
        localDate: todayDate,
        actualDate: null,
        providerVersion: ENVIRONMENT_PROVIDER_VERSION,
      });
    }

    // The eve of tomorrow's public holiday, under the holiday's own name with phase "eve", unless
    // today is an occasion already (Christmas Eve itself, New Year's Eve).
    if (results.length === 0) {
      for (const h of all) {
        const hDate = h.date.slice(0, 10);
        if (hDate !== tomorrowDate || h.substitute || mapHolidayKind(h.type) !== "public") continue;
        const id = `occasion:${countryLower}:${slugify(h.name)}`;
        if (seenIds.has(id)) continue;
        seenIds.add(id);
        results.push({
          id,
          label: h.name,
          phase: "eve",
          kind: "public",
          localDate: todayDate,
          actualDate: tomorrowDate,
          providerVersion: ENVIRONMENT_PROVIDER_VERSION,
        });
      }
    }

    return results;
  }
}
