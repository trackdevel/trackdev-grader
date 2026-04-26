package com.example.edge;

import java.io.BufferedReader;
import java.io.FileReader;

public class EdgeCases {

    // Empty method
    public void emptyMethod() {
    }

    // One-liner method
    public int oneLiner() { return 42; }

    // Nested if/else chain
    public String classify(int n) {
        if (n < 0) {
            return "negative";
        } else if (n == 0) {
            return "zero";
        } else if (n < 10) {
            return "small";
        } else {
            return "large";
        }
    }

    // Try-with-resources
    public String readFile(String path) {
        try (BufferedReader br = new BufferedReader(new FileReader(path))) {
            String line = br.readLine();
            return line;
        } catch (Exception e) {
            return null;
        }
    }

    // Switch statement
    public String dayName(int day) {
        switch (day) {
            case 1: return "Monday";
            case 2: return "Tuesday";
            default: return "Unknown";
        }
    }

    // Anonymous inner class
    public void anonymousClass() {
        Runnable r = new Runnable() {
            @Override
            public void run() {
                int x = 1;
                System.out.println(x);
            }
        };
        r.run();
    }

    // Static initializer
    static {
        System.out.println("loaded");
    }

    // Enum with body
    public enum Status {
        ACTIVE {
            @Override
            public String label() { return "Active"; }
        },
        INACTIVE {
            @Override
            public String label() { return "Inactive"; }
        };

        public abstract String label();
    }
}
