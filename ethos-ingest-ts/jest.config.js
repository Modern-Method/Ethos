/** @type {import("jest").Config} **/
module.exports = {
  testEnvironment: "node",
  testMatch: ["**/tests/**/*.test.ts", "**/*.spec.ts"],
  transform: {
    "^.+\\.tsx?$": ["ts-jest", {
      tsconfig: {
        module: "CommonJS",
        moduleResolution: "Node"
      }
    }],
  },
};
