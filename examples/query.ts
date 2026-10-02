const config = { base: "/api", headers: { Authorization: "example-token" } };

export async function loadUser(id: number) {
  const response = await fetch(`${config.base}/users/${id}`, {
    method: "GET",
    headers: config.headers,
  });
  return response.json();
}

export const saveUser = (user: { id: number; name: string }) =>
  client.post("/api/users", user);

const routes = [{ path: "/users/:id", load: loadUser }];
