// Matches CSI sequences such as colours (`ESC [ 32 m`); applied to visible rows only.
// biome-ignore lint/suspicious/noControlCharactersInRegex: matching ESC is the point
const CSI = /\u001b\[[0-9;?]*[ -/]*[@-~]/g;

export function stripAnsi(line: string): string {
  return line.includes("\u001b") ? line.replace(CSI, "") : line;
}
