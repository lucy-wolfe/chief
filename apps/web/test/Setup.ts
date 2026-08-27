// Enables React's act(...) support under Vitest's jsdom environment so that
// component tests can render with react-dom/client and flush updates.
// React reads this global to decide whether act(...) wraps updates.
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
