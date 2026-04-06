package org.example.drumcontroller;

import javax.sound.midi.*;
import java.io.File;
import java.io.IOException;
import java.nio.file.Path;
import java.util.*;
import org.springframework.web.bind.annotation.RequestMapping;
import org.example.drumcontroller.FileUploadController;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class Converter {
    private final StorageService storageService;

    public Converter(StorageService storageService) {
        this.storageService = storageService;
    }

    FileUploadController fileUploadController;
    private static final Map<Integer, String> GM_DRUM_MAP = Map.ofEntries(
            Map.entry(35, "Acoustic Bass Drum"),
            Map.entry(36, "Bass Drum 1"),
            Map.entry(37, "Side Stick"),
            Map.entry(38, "Acoustic Snare"),
            Map.entry(39, "Hand Clap"),
            Map.entry(40, "Electric Snare"),
            Map.entry(41, "Low Floor Tom"),
            Map.entry(42, "Closed Hi-Hat"),
            Map.entry(43, "High Floor Tom"),
            Map.entry(44, "Pedal Hi-Hat"),
            Map.entry(45, "Low Tom"),
            Map.entry(46, "Open Hi-Hat"),
            Map.entry(47, "Low-Mid Tom"),
            Map.entry(48, "Hi-Mid Tom"),
            Map.entry(49, "Crash Cymbal 1"),
            Map.entry(50, "High Tom"),
            Map.entry(51, "Ride Cymbal 1"),
            Map.entry(52, "Chinese Cymbal"),
            Map.entry(53, "Ride Bell"),
            Map.entry(54, "Tambourine"),
            Map.entry(55, "Splash Cymbal"),
            Map.entry(56, "Cowbell"),
            Map.entry(57, "Crash Cymbal 2"),
            Map.entry(58, "Vibraslap"),
            Map.entry(59, "Ride Cymbal 2"),
            Map.entry(60, "Hi Bongo"),
            Map.entry(61, "Low Bongo"),
            Map.entry(62, "Mute Hi Conga"),
            Map.entry(63, "Open Hi Conga"),
            Map.entry(64, "Low Conga"),
            Map.entry(65, "High Timbale"),
            Map.entry(66, "Low Timbale"),
            Map.entry(67, "High Agogo"),
            Map.entry(68, "Low Agogo"),
            Map.entry(69, "Cabasa"),
            Map.entry(70, "Maracas"),
            Map.entry(71, "Short Whistle"),
            Map.entry(72, "Long Whistle"),
            Map.entry(73, "Short Guiro"),
            Map.entry(74, "Long Guiro"),
            Map.entry(75, "Claves"),
            Map.entry(76, "Hi Wood Block"),
            Map.entry(77, "Low Wood Block"),
            Map.entry(78, "Mute Cuica"),
            Map.entry(79, "Open Cuica"),
            Map.entry(80, "Mute Triangle"),
            Map.entry(81, "Open Triangle")
    );
    public void convert(String filepath) throws MidiUnavailableException, InvalidMidiDataException, IOException {

        Sequencer sequencer = MidiSystem.getSequencer(false);
        sequencer.open();

        Sequence sequence = MidiSystem.getSequence(
                new File(filepath)
        );
        sequencer.setSequence(sequence);

        Set<Integer> drumChannels = detectDrumChannels(sequence);

        System.out.println("channels" + drumChannels);

        Receiver receiver = new Receiver() {
            @Override
            public void send(MidiMessage message, long timeStamp) {

                if (!(message instanceof ShortMessage sm)) return;

                if (sm.getCommand() == ShortMessage.NOTE_ON &&
                        sm.getData2() > 0 &&
                        drumChannels.contains(sm.getChannel())) {

                    int note = sm.getData1();
                    int velocity = sm.getData2();

                    String drum = GM_DRUM_MAP.getOrDefault(
                            note,
                            "Unknown Drum (" + note + ")"
                    );

                    System.out.println(
                            "Channel " + (sm.getChannel() + 1) +
                                    " | " + drum +
                                    " | Velocity " + velocity
                    );
                }
            }

            @Override
            public void close() {
            }
        };

        sequencer.getTransmitter().setReceiver(receiver);
        sequencer.start();
    }

    private static Set<Integer> detectDrumChannels(Sequence sequence) {

        Map<Integer, Integer> channelHitCount = new HashMap<>();

        for (Track track : sequence.getTracks()) {
            for (int i = 0; i < track.size(); i++) {
                MidiEvent event = track.get(i);
                MidiMessage msg = event.getMessage();

                if (msg instanceof ShortMessage sm &&
                        sm.getCommand() == ShortMessage.NOTE_ON &&
                        sm.getData2() > 0) {

                    int note = sm.getData1();
                    int channel = sm.getChannel();

                    if (note >= 35 && note <= 81) {
                        channelHitCount.merge(channel, 1, Integer::sum);
                    }
                }
            }
        }

        Set<Integer> drumChannels = new HashSet<>();
        for (var entry : channelHitCount.entrySet()) {
            if (entry.getValue() >= 5) { // threshold
                drumChannels.add(entry.getKey());
            }
        }

        return drumChannels;
    }

    @RequestMapping("/play")
    public String play()
            throws MidiUnavailableException, InvalidMidiDataException, IOException {

        Path path = storageService.getLastUploadedPath();

        if (path == null) {
            return "No file uploaded.";
        }

        convert(path.toString());

        return "played";
    }

}