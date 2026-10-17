// Ends a child and everything it started. On Windows there are no process groups:
// taskkill /T walks the tree. The host's Job Object (KILL_ON_JOB_CLOSE) is the backstop for
// anything this misses. On POSIX the child is spawned as a group leader, so the group is killed.

import { spawnSync } from "node:child_process";

export function treeKill(pid: number | undefined, platform: NodeJS.Platform = process.platform): boolean {
  if (!pid) return false;
  if (platform === "win32") {
    const r = spawnSync("taskkill", ["/T", "/F", "/PID", String(pid)], { windowsHide: true, stdio: "ignore" });
    return r.status === 0;
  }
  try {
    process.kill(-pid, "SIGKILL");
    return true;
  } catch {
    try {
      process.kill(pid, "SIGKILL");
      return true;
    } catch {
      return false;
    }
  }
}

export function isAlive(pid: number | undefined): boolean {
  if (!pid) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return (e as NodeJS.ErrnoException).code === "EPERM";
  }
}
