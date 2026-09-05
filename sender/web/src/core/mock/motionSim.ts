/**
 * Time-based interpolation along a queue of motions. Shared by the mock
 * Session (for a live DRO) and the mock VirtualMachine (for realistic
 * line-by-line acknowledgement timing).
 */

import type { MachineProfile } from "../types";
import type { Motion } from "./parser";
import { arcPoint, motionLength } from "./backplot";

export type SimPosition = { x: number; z: number };

export class MotionSim {
  private queue: Motion[] = [];
  private travelled = 0; // mm along current motion
  pos: SimPosition = { x: 0, z: 0 };
  paused = false;

  constructor(private profile: MachineProfile) {}

  get current(): Motion | null {
    return this.queue[0] ?? null;
  }

  get idle(): boolean {
    return this.queue.length === 0;
  }

  enqueue(motions: Motion[]): void {
    this.queue.push(...motions);
  }

  clear(): void {
    this.queue = [];
    this.travelled = 0;
  }

  private speedMmMin(m: Motion): number {
    if (m.kind === "rapid") return this.profile.rapidMmMin;
    const f = m.feedMmMin && m.feedMmMin > 0 ? m.feedMmMin : 60;
    return Math.min(f, this.profile.maxFeedMmMin);
  }

  /** Distance to go on the current motion, split per axis (mm). */
  dtg(): { x: number; z: number } {
    const m = this.current;
    if (!m) return { x: 0, z: 0 };
    return { x: m.x1 - this.pos.x, z: m.z1 - this.pos.z };
  }

  /**
   * Advance by dtMs. Returns the 0-based source lines whose last motion
   * completed during this step (for ack generation / line advance).
   */
  advance(dtMs: number): number[] {
    const done: number[] = [];
    if (this.paused) return done;
    let budget = Math.max(0, dtMs) / 60000; // minutes
    while (budget > 0) {
      const m = this.queue[0];
      if (!m) break;
      const len = motionLength(m);
      const v = this.speedMmMin(m);
      const remaining = len - this.travelled;
      const canGo = v * budget;
      if (canGo >= remaining - 1e-9 || len < 1e-9) {
        this.pos = { x: m.x1, z: m.z1 };
        budget -= v > 0 ? remaining / v : 0;
        this.queue.shift();
        this.travelled = 0;
        const next = this.queue[0];
        if (!next || next.line !== m.line) done.push(m.line);
      } else {
        this.travelled += canGo;
        const t = len > 0 ? this.travelled / len : 1;
        if (m.kind === "arc") {
          this.pos = arcPoint(m, t);
        } else {
          this.pos = {
            x: m.x0 + (m.x1 - m.x0) * t,
            z: m.z0 + (m.z1 - m.z0) * t,
          };
        }
        budget = 0;
      }
    }
    return done;
  }
}
