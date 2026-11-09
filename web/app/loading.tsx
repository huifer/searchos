import { Skeleton } from "@/components/ui";

export default function Loading() {
  return (
    <div className="stack gap-4 p-6">
      <Skeleton lines={2} />
      <Skeleton lines={6} />
    </div>
  );
}
