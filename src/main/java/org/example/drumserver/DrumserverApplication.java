package org.example.drumserver;

import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;
import org.springframework.web.bind.annotation.*;

import com.pi4j.Pi4J;
import com.pi4j.io.gpio.digital.DigitalOutput;
import com.pi4j.io.gpio.digital.DigitalState;

@SpringBootApplication
@RestController
public class DrumserverApplication {

    private static final int PIN_LED = 17;
    private static DigitalOutput led;

    public static void main(String[] args) throws Exception {
        var pi4j = Pi4J.newAutoContext();
        led = pi4j.digitalOutput().create(PIN_LED);
        led.high();
        SpringApplication.run(DrumserverApplication.class, args);
    }

    @PostMapping("/")
    public String blinkOnPost() throws InterruptedException {
        for (int i = 0; i < 5; i++) {
            led.low();
            Thread.sleep(200);
            led.high();
            Thread.sleep(200);
        }
        return "Blink done!";
    }

    @GetMapping("/")
    public String hello(@RequestParam(value = "name", defaultValue = "World") String name) {
        return String.format("Hello %s!", name);
    }
}