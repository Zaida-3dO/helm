// The wordmark: "Helm" in the bundled Source Sans 3 SemiBold, letter-spacing -0.3 px,
// loaded with font-display:block so it never flashes in a fallback face. Dark mode fills the glyphs
// with the chrome gradient. Size is the caller's: set font-size on it or its parent.

export function Wordmark({ className, children = 'Helm' }: { className?: string; children?: string }) {
  return <span className={`ui-wordmark${className ? ` ${className}` : ''}`}>{children}</span>;
}
