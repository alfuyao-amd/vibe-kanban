import { ReviewProvider } from '@/shared/hooks/ReviewProvider';
import { useWorkspaceContext } from '@/shared/hooks/useWorkspaceContext';
import { WorkspacesMainContainer } from '@/pages/workspaces/WorkspacesMainContainer';

/**
 * Bare-minimum chat surface for embedding into non-workspace routes.
 *
 * Renders just the conversation list + composer that `WorkspacesLayout`
 * normally wraps in resizable panels alongside the workspace sidebar,
 * changes panel, preview, etc. The lead-agent route uses this so the
 * embedded chat stays "lean" — no workspaces sidebar (the user is in a
 * project context, not picking workspaces) and no git/diff panel
 * (procedure runs surface diffs in their own pages).
 *
 * Pulls all data from the surrounding `WorkspaceProvider`. Mount inside an
 * override-props `WorkspaceProvider` to pin the workspace + session.
 */
export function EmbeddedChatPane() {
  const {
    workspace: selectedWorkspace,
    isLoading,
    selectedSession,
    selectedSessionId,
    sessions,
    isSessionsLoading,
    selectSession,
    repos,
    isNewSessionMode,
    startNewSession,
  } = useWorkspaceContext();

  return (
    <ReviewProvider workspaceId={selectedWorkspace?.id}>
      <div className="flex flex-col h-full min-h-0">
        <WorkspacesMainContainer
          selectedWorkspace={selectedWorkspace ?? null}
          selectedSession={selectedSession}
          selectedSessionId={selectedSessionId}
          sessions={sessions}
          repos={repos}
          onSelectSession={selectSession}
          isLoading={isLoading}
          isSessionsLoading={isSessionsLoading}
          isNewSessionMode={isNewSessionMode}
          onStartNewSession={startNewSession}
        />
      </div>
    </ReviewProvider>
  );
}
