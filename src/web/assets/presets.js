// Presets: named sets of plugins (each with its options) to run together. Built-in ones ship
// with fastvol; the user's own live in ~/.fvol/presets/<id>.json on the server.

import { api, store } from './core.js';

const P = (plugin, args = {}) => ({ plugin, args });

// os "any": one preset for every system; "byOs" picks its plugins for the open image's OS
export const BUILTIN = [
  { id: 'builtin:system-info', os: 'any', name: 'System Information',
    byOs: {
      windows: [P('windows.info.Info'), P('windows.sessions.Sessions'), P('windows.envars.Envars'), P('windows.registry.hivelist.HiveList')],
      linux: [P('banners.Banners'), P('linux.boottime.Boottime'), P('linux.ip.Addr'), P('linux.ip.Link'), P('linux.mountinfo.MountInfo'), P('linux.kmsg.Kmsg')],
      mac: [P('banners.Banners'), P('mac.ifconfig.Ifconfig'), P('mac.mount.Mount'), P('mac.dmesg.Dmesg')],
    } },
  { id: 'builtin:windows-triage', os: 'windows', name: 'Windows Triage',
    plugins: [P('windows.pslist.PsList'), P('windows.pstree.PsTree'), P('windows.psscan.PsScan'), P('windows.cmdline.CmdLine'),
      P('windows.netscan.NetScan'), P('windows.svcscan.SvcScan'), P('windows.malware.psxview.PsXView'), P('windows.malware.malfind.Malfind'),
      P('windows.cmdscan.CmdScan'), P('windows.consoles.Consoles')] },
  { id: 'builtin:windows-malware-persistence', os: 'windows', name: 'Windows Malware and Persistence',
    plugins: [P('windows.malware.hollowprocesses.HollowProcesses'), P('windows.malware.ldrmodules.LdrModules'), P('windows.malware.processghosting.ProcessGhosting'),
      P('windows.malware.pebmasquerade.PebMasquerade'), P('windows.malware.suspicious_threads.SuspiciousThreads'),
      P('windows.malware.drivermodule.DriverModule'), P('windows.malware.svcdiff.SvcDiff'), P('windows.callbacks.Callbacks'), P('windows.ssdt.SSDT'),
      P('windows.registry.scheduled_tasks.ScheduledTasks'), P('windows.registry.userassist.UserAssist'), P('windows.shimcachemem.ShimcacheMem'),
      P('windows.registry.amcache.Amcache')] },
  { id: 'builtin:linux-triage', os: 'linux', name: 'Linux Triage',
    plugins: [P('linux.pslist.PsList'), P('linux.pstree.PsTree'), P('linux.psscan.PsScan'), P('linux.psaux.PsAux'), P('linux.bash.Bash'),
      P('linux.envars.Envars'), P('linux.sockstat.Sockstat'), P('linux.lsmod.Lsmod'), P('linux.malware.malfind.Malfind'),
      P('linux.malware.check_creds.Check_creds')] },
  { id: 'builtin:linux-rootkit', os: 'linux', name: 'Linux Rootkit Hunt',
    plugins: [P('linux.malware.check_syscall.Check_syscall'), P('linux.malware.check_idt.Check_idt'), P('linux.malware.check_afinfo.Check_afinfo'),
      P('linux.malware.check_modules.Check_modules'), P('linux.malware.hidden_modules.Hidden_modules'), P('linux.malware.modxview.Modxview'),
      P('linux.malware.keyboard_notifiers.Keyboard_notifiers'), P('linux.malware.netfilter.Netfilter'), P('linux.malware.tty_check.Tty_Check'),
      P('linux.malware.process_spoofing.ProcessSpoofing'), P('linux.tracing.ftrace.CheckFtrace'), P('linux.tracing.tracepoints.CheckTracepoints'),
      P('linux.ebpf.EBPF')] },
  { id: 'builtin:mac-triage', os: 'mac', name: 'macOS Triage',
    plugins: [P('mac.pslist.PsList'), P('mac.pstree.PsTree'), P('mac.psaux.Psaux'), P('mac.bash.Bash'), P('mac.netstat.Netstat'),
      P('mac.lsof.Lsof'), P('mac.lsmod.Lsmod'), P('mac.malfind.Malfind'), P('mac.check_syscall.Check_syscall'), P('mac.check_sysctl.Check_sysctl'),
      P('mac.check_trap_table.Check_trap_table'), P('mac.kauth_listeners.Kauth_listeners'), P('mac.trustedbsd.Trustedbsd'),
      P('mac.socket_filters.Socket_filters')] },
];

/** Built-in presets for `os` (all systems when unknown), with the plugins this build has. */
export function builtinPresets(os) {
  const out = [];
  for (const p of BUILTIN) {
    if (os && p.os !== os && p.os !== 'any') continue;
    const plugins = p.byOs ? (os ? p.byOs[os] || [] : []) : p.plugins;
    const have = plugins.filter(e => store.pluginMap.has(e.plugin));
    if (have.length) out.push({ ...p, builtin: true, plugins: have });
  }
  return out;
}

// the user's presets, as last listed by the server
export const userPresets = { list: [], errors: [], dir: '', loaded: false, error: null };

export async function loadUserPresets() {
  try {
    const r = await api('presets');
    Object.assign(userPresets, { list: r.presets, errors: r.errors, dir: r.dir, loaded: true, error: null });
  } catch (e) { Object.assign(userPresets, { loaded: true, error: e.message }); }
  return userPresets;
}

/** Save a preset from the current selection (a Map plugin -> options). */
export function savePreset({ name, os, selection, overwrite = false }) {
  const plugins = [...selection].map(([plugin, args]) => ({ plugin, args }));
  return api('presets', { method: 'POST', body: { name, os, plugins, overwrite } });
}

export function deletePreset(id) {
  return api('presets/' + encodeURIComponent(id), { method: 'DELETE' });
}
