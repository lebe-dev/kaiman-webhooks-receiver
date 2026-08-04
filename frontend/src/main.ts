import { mount } from "svelte";

import "./styles/global.css";

import App from "./components/App.svelte";

const target = document.getElementById("app");

if (!target) {
  throw new Error("Mount target #app not found in index.html");
}

export const app = mount(App, { target });
