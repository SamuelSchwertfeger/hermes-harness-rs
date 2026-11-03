"""Deterministic, network-free gateway for disposable supervised qualification."""

from dataclasses import dataclass, field


@dataclass(slots=True)
class ScriptedSupervisedGateway:
    """In-memory gateway double. Never creates a transport or provider."""

    worker_outcome: str = 'completed'
    reviewer_outcome: str = 'completed'
    fail_submission_role: str = ''
    calls: list[str] = field(default_factory=list)
    sessions: dict[str, str] = field(default_factory=dict)

    async def rpc(self, method: str, **params: object) -> object:
        """Return scripted admissions, queued receipts and terminal evidence."""
        self.calls.append(method)
        session = params.get('session_id')
        if not isinstance(session, str) or not session:
            msg = 'Scripted gateway requires session identity'
            raise ValueError(msg)
        role = 'worker' if session.startswith('worker-') else 'reviewer'
        if method == 'prompt.submit':
            if role == self.fail_submission_role:
                msg = 'Scripted ambiguous submission failure'
                raise OSError(msg)
            admission = f'scripted-{session}'
            self.sessions[admission] = session
            return {'admission_id': admission, 'status': 'queued'}
        receipt_admission = params.get('admission_id')
        if (
            not isinstance(receipt_admission, str)
            or self.sessions.get(receipt_admission) != session
        ):
            msg = 'Unknown scripted admission identity'
            raise ValueError(msg)
        if method != 'prompt.receipt':
            msg = 'Only submit and read-only receipts are supported'
            raise ValueError(msg)
        if params.get('include_result') is not True:
            return {'admission_id': receipt_admission, 'status': 'queued'}
        outcome = self.worker_outcome if role == 'worker' else self.reviewer_outcome
        return {
            'admission_id': receipt_admission,
            'status': 'terminal',
            'outcome': outcome,
            'ref': {'session_id': session},
            'result': {'completed': outcome == 'completed', 'final_response': 'scripted evidence'},
        }


# Temporary exports for legacy module import compatibility.
__all__ = [
    'ScriptedSupervisedGateway',
    'dataclass',
    'field',
]
