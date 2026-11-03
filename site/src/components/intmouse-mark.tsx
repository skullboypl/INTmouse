/** Znak: INT w akcencie, reszta nazwy w kolorze tekstu. */
export function INTmouseMark({ className = "" }: { className?: string }) {
  return (
    <span className={`inline-flex items-baseline font-display text-lg font-medium tracking-tight text-fg ${className}`}>
      <span>
        <span className="text-accent">INT</span>mouse
      </span>
    </span>
  );
}
