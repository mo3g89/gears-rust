import { Navigate } from 'react-router-dom';

export function CustomPlansPage() {
  return <Navigate to="/plans?tab=custom" replace />;
}
