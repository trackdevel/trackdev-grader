import type { ParityResult } from "../logic/parity";

type Props = {
  parity: ParityResult;
};

export default function ParityBanner({ parity }: Props) {
  const className =
    parity.state === "standard" || parity.state === "unchecked"
      ? "banner ok"
      : parity.state === "edited"
        ? "banner tuned"
        : "banner error";

  return (
    <div className={className} role="status">
      {parity.message}
    </div>
  );
}
