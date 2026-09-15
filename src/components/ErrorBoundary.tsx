import { Component, type ErrorInfo, type ReactNode } from "react";
import { logUi } from "../lib/activityLog";

interface ErrorBoundaryProps {
  children: ReactNode;
}

interface ErrorBoundaryState {
  error: Error | null;
}

/**
 * Catches render crashes so a white screen leaves a trace in the activity
 * log instead of disappearing silently.
 */
export class ErrorBoundary extends Component<
  ErrorBoundaryProps,
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    const where = info.componentStack?.trim().split("\n")[0] ?? "";
    logUi(
      "react",
      `render 崩溃：${error.message}${where ? ` @ ${where}` : ""}`,
      "error",
    );
  }

  render() {
    if (this.state.error) {
      return (
        <div className="error-boundary">
          <p>界面出现异常</p>
          <p className="error-boundary-detail">{this.state.error.message}</p>
          <button
            type="button"
            className="btn"
            onClick={() => this.setState({ error: null })}
          >
            重试
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
