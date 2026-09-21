/**
 * Nothing but the base path, deliberately. Every URL this app produces must go through
 * `import.meta.env.BASE_URL`; nothing may hardcode a root-absolute path.
 */
export default function App() {
  return (
    <main>
      <h1>osu! corner</h1>
      <p>Scaffolding. Nothing is built yet.</p>
      <p>
        Mounted at <code>{import.meta.env.BASE_URL}</code>
      </p>
    </main>
  );
}
