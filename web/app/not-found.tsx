import Link from "next/link";

export default function NotFound() {
  return (
    <div className="stack items-center justify-center py-24 text-center">
      <span className="font-mono text-[11px] text-muted-foreground">404</span>
      <h1 className="text-2xl font-semibold tracking-tight">Page not found</h1>
      <p className="max-w-md text-sm text-muted-foreground">
        The console route does not exist. Return to the system overview or pick a
        surface from the navigation.
      </p>
      <Link href="/" className="inline-link mt-4">
        Back to overview
      </Link>
    </div>
  );
}
