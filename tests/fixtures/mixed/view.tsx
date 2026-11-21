export const View = ({ ok }: { ok?: boolean }) => (
    <div>{ok && <span>{ok ? "yes" : "no"}</span>}</div>
);
